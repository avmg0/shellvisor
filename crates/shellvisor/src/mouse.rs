//! Mouse events from the real terminal (always SGR-encoded, Shellvisor asks
//! for that) and re-encoding them for a program inside a pane, relative to
//! the pane's position and in the protocol the program asked for.

use vt100::{MouseProtocolEncoding as Enc, MouseProtocolMode as Mode};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseEv {
    /// Raw SGR button code: low 2 bits = button, +4 shift, +8 alt, +16 ctrl,
    /// +32 motion, 64/65 = wheel up/down.
    pub code: u16,
    /// 0-based screen column / row.
    pub x: u16,
    pub y: u16,
    pub release: bool,
}

impl MouseEv {
    /// Parse `ESC [ < code ; x ; y (M|m)`.
    pub fn parse_sgr(tok: &[u8]) -> Option<MouseEv> {
        let body = tok.strip_prefix(b"\x1b[<")?;
        let (last, fields) = body.split_last()?;
        let release = match last {
            b'M' => false,
            b'm' => true,
            _ => return None,
        };
        let mut nums = std::str::from_utf8(fields)
            .ok()?
            .split(';')
            .map(|n| n.parse::<u16>().ok());
        let code = nums.next()??;
        let x = nums.next()??.checked_sub(1)?;
        let y = nums.next()??.checked_sub(1)?;
        Some(MouseEv {
            code,
            x,
            y,
            release,
        })
    }

    /// +1 wheel up, -1 wheel down.
    pub fn wheel(&self) -> Option<isize> {
        match self.code & !0b11100 {
            64 => Some(1),
            65 => Some(-1),
            _ => None,
        }
    }

    /// Sideways wheel (trackpads, tilt wheels): -1 left, +1 right.
    pub fn hwheel(&self) -> Option<isize> {
        match self.code & !0b11100 {
            66 => Some(-1),
            67 => Some(1),
            _ => None,
        }
    }

    /// Shift was held.
    pub fn shift(&self) -> bool {
        self.code & 4 != 0
    }

    /// Moving with the left button held.
    pub fn is_left_drag(&self) -> bool {
        self.code & 32 != 0
            && self.code & 0b11 == 0
            && self.wheel().is_none()
            && self.hwheel().is_none()
    }

    pub fn is_motion(&self) -> bool {
        self.code & 32 != 0 && self.wheel().is_none() && self.hwheel().is_none()
    }

    /// A plain left-button press.
    pub fn is_left_press(&self) -> bool {
        !self.release && !self.is_motion() && self.wheel().is_none() && self.code & 0b11 == 0
    }

    /// Encode for a program that asked for `mode`/`enc`, at pane-local (x, y).
    /// None if the program didn't ask for this kind of event.
    pub fn encode(&self, x: u16, y: u16, mode: Mode, enc: Enc) -> Option<Vec<u8>> {
        let wanted = match mode {
            Mode::None => false,
            Mode::Press => !self.release && !self.is_motion(),
            Mode::PressRelease => !self.is_motion(),
            Mode::ButtonMotion => !self.is_motion() || self.code & 0b11 != 3,
            Mode::AnyMotion => true,
        };
        if !wanted {
            return None;
        }
        match enc {
            Enc::Sgr => Some(
                format!(
                    "\x1b[<{};{};{}{}",
                    self.code,
                    x + 1,
                    y + 1,
                    if self.release { 'm' } else { 'M' }
                )
                .into_bytes(),
            ),
            Enc::Default | Enc::Utf8 => {
                // Legacy encodings report a release as button 3.
                let code = if self.release {
                    (self.code & !0b11) | 3
                } else {
                    self.code
                };
                let mut out = b"\x1b[M".to_vec();
                push_coord(&mut out, code as u32, enc)?;
                push_coord(&mut out, x as u32 + 1, enc)?;
                push_coord(&mut out, y as u32 + 1, enc)?;
                Some(out)
            }
        }
    }
}

fn push_coord(out: &mut Vec<u8>, v: u32, enc: Enc) -> Option<()> {
    let n = v + 32;
    match enc {
        Enc::Utf8 => {
            let c = char::from_u32(n)?;
            let mut buf = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        }
        _ => {
            if n > 255 {
                return None; // can't be expressed in the old protocol
            }
            out.push(n as u8);
        }
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse() {
        assert_eq!(
            MouseEv::parse_sgr(b"\x1b[<0;10;5M"),
            Some(MouseEv {
                code: 0,
                x: 9,
                y: 4,
                release: false
            })
        );
        assert!(MouseEv::parse_sgr(b"\x1b[<0;10;5m").unwrap().release);
        assert_eq!(
            MouseEv::parse_sgr(b"\x1b[<64;1;1M").unwrap().wheel(),
            Some(1)
        );
        assert_eq!(
            MouseEv::parse_sgr(b"\x1b[<65;1;1M").unwrap().wheel(),
            Some(-1)
        );
        assert_eq!(MouseEv::parse_sgr(b"\x1b[A"), None);
        assert_eq!(MouseEv::parse_sgr(b"\x1b[<0;0;5M"), None);
    }

    #[test]
    fn reencode_for_pane() {
        let e = MouseEv::parse_sgr(b"\x1b[<0;61;6M").unwrap();
        // pane starts at column 50, row 2 → local (10, 3)
        assert_eq!(
            e.encode(e.x - 50, e.y - 2, Mode::PressRelease, Enc::Sgr)
                .unwrap(),
            b"\x1b[<0;11;4M"
        );
        assert_eq!(
            e.encode(10, 3, Mode::PressRelease, Enc::Default).unwrap(),
            b"\x1b[M +$"
        );
        let up = MouseEv { release: true, ..e };
        assert_eq!(
            up.encode(10, 3, Mode::PressRelease, Enc::Default).unwrap()[3],
            32 + 3
        );
        assert_eq!(up.encode(10, 3, Mode::Press, Enc::Sgr), None);
    }

    #[test]
    fn motion_filtering() {
        let drag = MouseEv {
            code: 32,
            x: 1,
            y: 1,
            release: false,
        }; // left held, moving
        let hover = MouseEv {
            code: 35,
            x: 1,
            y: 1,
            release: false,
        }; // no button, moving
        assert!(drag.encode(1, 1, Mode::PressRelease, Enc::Sgr).is_none());
        assert!(drag.encode(1, 1, Mode::ButtonMotion, Enc::Sgr).is_some());
        assert!(hover.encode(1, 1, Mode::ButtonMotion, Enc::Sgr).is_none());
        assert!(hover.encode(1, 1, Mode::AnyMotion, Enc::Sgr).is_some());
    }

    #[test]
    fn big_coordinates_need_sgr() {
        let e = MouseEv {
            code: 0,
            x: 300,
            y: 1,
            release: false,
        };
        assert!(e.encode(300, 1, Mode::PressRelease, Enc::Default).is_none());
        assert!(e.encode(300, 1, Mode::PressRelease, Enc::Utf8).is_some());
        assert!(e.encode(300, 1, Mode::PressRelease, Enc::Sgr).is_some());
    }
}
