//! `sv terminal-colors [undo]`: give Terminal.app's current profile the
//! design's background and text colours.
//!
//! Terminal.app before macOS 26 shows 256 colours, and 256 colours have no
//! very dark blue — only greys. So in 256 colours Shellvisor leaves the page
//! background to the terminal, and this sets the terminal's own background
//! to the design's `#0b0e12` (text `#d6dbe3`). The colours it replaces are
//! saved first; `sv terminal-colors undo` puts them back.
//!
//! `sv terminal-colors profile` instead imports the whole Shellvisor profile
//! (packaging/terminal/Shellvisor.terminal: background, text, the 16 ANSI
//! colours, JetBrains Mono), which Terminal then offers in Settings →
//! Profiles, where it can be made the default.

use std::process::Command;

const BACKUP: &str = "terminal-colors-backup.txt";

/// The Shellvisor profile for Terminal.app (made by scripts/terminal-profile.py).
const PROFILE: &[u8] = include_bytes!("../../../packaging/terminal/Shellvisor.terminal");

/// AppleScript colours are 16-bit per channel.
fn c16((r, g, b): (u8, u8, u8)) -> String {
    format!(
        "{{{}, {}, {}}}",
        r as u32 * 257,
        g as u32 * 257,
        b as u32 * 257
    )
}

const TARGET: &str = "current settings of selected tab of front window";

fn osascript(script: &str) -> Result<String, String> {
    let out = Command::new("osascript")
        .arg("-e")
        .arg(script)
        .output()
        .map_err(|e| format!("osascript: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// The script that sets the profile's colours: bg, text, bold, cursor.
pub fn set_script(bg: &str, text: &str, bold: &str, cursor: &str) -> String {
    format!(
        "tell application \"Terminal\"\n\
         set s to {TARGET}\n\
         set background color of s to {bg}\n\
         set normal text color of s to {text}\n\
         set bold text color of s to {bold}\n\
         set cursor color of s to {cursor}\n\
         return name of s\n\
         end tell"
    )
}

/// "1, 2, 3, 4, 5, 6" → ["{1, 2, 3}", "{4, 5, 6}"]
fn triples(list: &str) -> Vec<String> {
    let n: Vec<&str> = list.split(',').map(str::trim).collect();
    n.chunks(3)
        .filter(|c| c.len() == 3)
        .map(|c| format!("{{{}, {}, {}}}", c[0], c[1], c[2]))
        .collect()
}

pub fn run(args: &[String]) -> i32 {
    if !cfg!(target_os = "macos") {
        eprintln!("sv terminal-colors: this is for Terminal.app on macOS");
        return 1;
    }
    let backup = crate::paths::state_dir().join(BACKUP);
    match args.first().map(String::as_str) {
        Some("profile") => {
            let path = std::env::temp_dir().join("Shellvisor.terminal");
            if let Err(e) = std::fs::write(&path, PROFILE) {
                eprintln!("sv terminal-colors: {}: {e}", path.display());
                return 1;
            }
            // opening a .terminal file imports it (and opens a window with it)
            match Command::new("open").arg(&path).status() {
                Ok(st) if st.success() => {
                    println!(
                        "imported the Shellvisor profile into Terminal (a new window shows it)"
                    );
                    println!(
                        "make it the default: Terminal → Settings → Profiles → Shellvisor → Default"
                    );
                    0
                }
                _ => {
                    eprintln!("sv terminal-colors: couldn't open {}", path.display());
                    1
                }
            }
        }
        Some("undo") => {
            let Ok(saved) = std::fs::read_to_string(&backup) else {
                eprintln!("sv terminal-colors: nothing to undo");
                return 1;
            };
            let t = triples(&saved);
            if t.len() != 4 {
                eprintln!("sv terminal-colors: the saved colours look wrong ({saved})");
                return 1;
            }
            match osascript(&set_script(&t[0], &t[1], &t[2], &t[3])) {
                Ok(name) => {
                    let _ = std::fs::remove_file(&backup);
                    println!("put back the colours of the \"{name}\" profile");
                    0
                }
                Err(e) => {
                    eprintln!("sv terminal-colors: {e}");
                    1
                }
            }
        }
        None => {
            let tp = std::env::var("SHELLVISOR_OUTER_TERM").unwrap_or_default();
            if !tp.is_empty() && tp != "Apple_Terminal" {
                eprintln!(
                    "sv terminal-colors: this is for Terminal.app (you're in {tp}, which shows 24-bit colour)"
                );
                return 1;
            }
            if !backup.exists() {
                let got = osascript(&format!(
                    "tell application \"Terminal\" to get {{background color, normal text color, bold text color, cursor color}} of {TARGET}"
                ));
                match got {
                    Ok(list) => {
                        let _ = std::fs::create_dir_all(crate::paths::state_dir());
                        let _ = std::fs::write(&backup, list);
                    }
                    Err(e) => {
                        eprintln!("sv terminal-colors: {e}");
                        return 1;
                    }
                }
            }
            use crate::palette::{BG, TEXT};
            match osascript(&set_script(&c16(BG), &c16(TEXT), &c16(TEXT), &c16(TEXT))) {
                Ok(name) => {
                    println!(
                        "the \"{name}\" profile now has the design's background #0b0e12 and text #d6dbe3"
                    );
                    println!(
                        "(other windows with this profile change too) · undo: sv terminal-colors undo"
                    );
                    0
                }
                Err(e) => {
                    eprintln!("sv terminal-colors: {e}");
                    1
                }
            }
        }
        Some(other) => {
            eprintln!("sv terminal-colors: `{other}` — no argument, profile, or undo");
            2
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripts() {
        assert_eq!(c16((0x0b, 0x0e, 0x12)), "{2827, 3598, 4626}");
        assert_eq!(
            triples("1, 2, 3, 4, 5, 6, 7"),
            vec!["{1, 2, 3}".to_string(), "{4, 5, 6}".to_string()]
        );
        let s = set_script("{1, 2, 3}", "{4, 5, 6}", "{4, 5, 6}", "{7, 8, 9}");
        assert!(s.contains("set background color of s to {1, 2, 3}"));
        assert!(s.contains("selected tab of front window"));
        let p = String::from_utf8_lossy(PROFILE);
        assert!(p.contains("<string>Shellvisor</string>") && p.contains("BackgroundColor"));
    }
}
