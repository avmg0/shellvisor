//! `sv serial [device] [baud]`: a serial console, like PuTTY's Serial
//! connection type or `cu` / `screen /dev/… 115200` — for routers, switches,
//! Arduino and other boards.
//!
//! With no device it lists the serial ports. Inside Shellvisor the console
//! opens in a new tab named after the port. Keys go to the device as you
//! type them; `~.` at the start of a line quits (as in `cu` and ssh).

use std::io::{Read, Write};
use std::os::fd::AsRawFd;

/// Serial ports on this machine (USB adapters, boards, built-in ports).
pub fn ports() -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir("/dev")
        .map(|d| {
            d.filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| {
                    (n.starts_with("cu.")
                        && !n.contains("Bluetooth-Incoming")
                        && !n.contains("debug-console"))
                        || n.starts_with("ttyUSB")
                        || n.starts_with("ttyACM")
                        || n.starts_with("ttyAMA")
                        || n.starts_with("rfcomm")
                })
                .map(|n| format!("/dev/{n}"))
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// The termios speed for a baud rate (None: not one the system knows).
pub fn speed(baud: u32) -> Option<libc::speed_t> {
    #[cfg(target_os = "macos")]
    {
        // macOS takes the number itself
        [
            300u32, 1200, 2400, 4800, 9600, 19200, 38400, 57600, 115200, 230400,
        ]
        .contains(&baud)
        .then_some(baud as libc::speed_t)
    }
    #[cfg(not(target_os = "macos"))]
    {
        Some(match baud {
            300 => libc::B300,
            1200 => libc::B1200,
            2400 => libc::B2400,
            4800 => libc::B4800,
            9600 => libc::B9600,
            19200 => libc::B19200,
            38400 => libc::B38400,
            57600 => libc::B57600,
            115200 => libc::B115200,
            230400 => libc::B230400,
            460800 => libc::B460800,
            921600 => libc::B921600,
            _ => return None,
        })
    }
}

/// Raw mode for a terminal fd; returns the settings to put back.
fn make_raw(fd: i32, baud: Option<libc::speed_t>) -> std::io::Result<libc::termios> {
    unsafe {
        let mut t: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(fd, &mut t) != 0 {
            return Err(std::io::Error::last_os_error());
        }
        let old = t;
        libc::cfmakeraw(&mut t);
        if let Some(s) = baud {
            t.c_cflag |= libc::CLOCAL | libc::CREAD;
            libc::cfsetispeed(&mut t, s);
            libc::cfsetospeed(&mut t, s);
        }
        t.c_cc[libc::VMIN] = 1;
        t.c_cc[libc::VTIME] = 0;
        if libc::tcsetattr(fd, libc::TCSANOW, &t) != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(old)
    }
}

/// Watch the typed keys for `~.` at the start of a line.
#[derive(Default)]
pub struct Escape {
    at_line_start: bool,
    tilde: bool,
}

impl Escape {
    pub fn new() -> Self {
        Escape {
            at_line_start: true,
            tilde: false,
        }
    }

    /// The bytes to send for what was typed, and whether to quit.
    pub fn filter(&mut self, input: &[u8]) -> (Vec<u8>, bool) {
        let mut out = Vec::with_capacity(input.len());
        for &b in input {
            if self.tilde {
                self.tilde = false;
                if b == b'.' {
                    return (out, true);
                }
                out.push(b'~');
                if b == b'~' {
                    self.at_line_start = false;
                    continue; // ~~ sends one ~
                }
            } else if self.at_line_start && b == b'~' {
                self.tilde = true;
                continue;
            }
            out.push(b);
            self.at_line_start = b == b'\r' || b == b'\n';
        }
        (out, false)
    }
}

/// Run the console in this terminal until `~.` or the device goes away.
pub fn console(device: &str, baud: u32) -> i32 {
    let Some(sp) = speed(baud) else {
        eprintln!("sv serial: {baud} isn't a baud rate this system knows (try 9600 or 115200)");
        return 2;
    };
    let mut dev = match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags_noctty()
        .open(device)
    {
        Ok(f) => f,
        Err(e) => {
            eprintln!("sv serial: {device}: {e}");
            if e.kind() == std::io::ErrorKind::PermissionDenied {
                eprintln!(
                    "  (on Linux, add yourself to the dialout group: sudo usermod -aG dialout $USER)"
                );
            }
            return 1;
        }
    };
    if let Err(e) = make_raw(dev.as_raw_fd(), Some(sp)) {
        eprintln!("sv serial: {device}: can't set {baud} baud: {e}");
        return 1;
    }
    let stdin_fd = std::io::stdin().as_raw_fd();
    let old = make_raw(stdin_fd, None).ok();
    let mut out = std::io::stdout();
    let _ = write!(
        out,
        "connected to {device} at {baud} baud · ~. at the start of a line quits\r\n"
    );
    let _ = out.flush();
    let mut esc = Escape::new();
    let mut buf = [0u8; 4096];
    let code = loop {
        let mut fds = [
            libc::pollfd {
                fd: stdin_fd,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: dev.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let n = unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) };
        if n < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            break 1;
        }
        if fds[1].revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0 {
            match dev.read(&mut buf) {
                Ok(0) | Err(_) => {
                    let _ = write!(out, "\r\n{device} went away\r\n");
                    break 1;
                }
                Ok(n) => {
                    let _ = out.write_all(&buf[..n]);
                    let _ = out.flush();
                }
            }
        }
        if fds[0].revents & libc::POLLIN != 0 {
            match std::io::stdin().read(&mut buf) {
                Ok(0) | Err(_) => break 0,
                Ok(n) => {
                    let (send, quit) = esc.filter(&buf[..n]);
                    if !send.is_empty() && dev.write_all(&send).is_err() {
                        break 1;
                    }
                    if quit {
                        let _ = write!(out, "\r\ndisconnected from {device}\r\n");
                        break 0;
                    }
                }
            }
        }
    };
    if let Some(t) = old {
        unsafe {
            libc::tcsetattr(stdin_fd, libc::TCSANOW, &t);
        }
    }
    code
}

trait NoCtty {
    fn custom_flags_noctty(&mut self) -> &mut Self;
}

impl NoCtty for std::fs::OpenOptions {
    fn custom_flags_noctty(&mut self) -> &mut Self {
        use std::os::unix::fs::OpenOptionsExt;
        self.custom_flags(libc::O_NOCTTY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tilde_dot_quits() {
        let mut e = Escape::new();
        assert_eq!(e.filter(b"ls\r"), (b"ls\r".to_vec(), false));
        assert_eq!(e.filter(b"~."), (Vec::new(), true));
        let mut e = Escape::new();
        assert_eq!(
            e.filter(b"a~.b"),
            (b"a~.b".to_vec(), false),
            "only at a line start"
        );
        let mut e = Escape::new();
        assert_eq!(e.filter(b"~~x"), (b"~x".to_vec(), false));
        assert!(speed(115200).is_some() && speed(12345).is_none());
    }
}
