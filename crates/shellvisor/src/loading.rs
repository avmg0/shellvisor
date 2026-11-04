//! Loading indicators: the spinner (style chosen in settings) and the
//! filling progress bar used for downloads and installs.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::time::Instant;

/// Spinner styles offered in settings: (name, frames).
pub const STYLES: [(&str, &[&str]); 6] = [
    ("circle", &["◐", "◓", "◑", "◒"]),
    ("dots", &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]),
    ("arc", &["◜", "◠", "◝", "◞", "◡", "◟"]),
    ("line", &["|", "/", "-", "\\"]),
    ("pulse", &["·", "•", "●", "•"]),
    ("bounce", &["⠁", "⠂", "⠄", "⡀", "⢀", "⠠", "⠐", "⠈"]),
];

static STYLE: AtomicU8 = AtomicU8::new(0);
static LOW_POWER: AtomicBool = AtomicBool::new(false);
static BAR: AtomicBool = AtomicBool::new(true);

fn epoch() -> Instant {
    static E: OnceLock<Instant> = OnceLock::new();
    *E.get_or_init(Instant::now)
}

/// Use this spinner style (a name from `STYLES`; unknown names: circle).
pub fn set_style(name: &str) {
    let i = STYLES.iter().position(|(n, _)| *n == name).unwrap_or(0);
    STYLE.store(i as u8, Ordering::Relaxed);
}

pub fn set_low_power(on: bool) {
    LOW_POWER.store(on, Ordering::Relaxed);
}

/// Show a filling bar for downloads (or the percentage only).
pub fn set_bar(on: bool) {
    BAR.store(on, Ordering::Relaxed);
}

pub fn bar_on() -> bool {
    BAR.load(Ordering::Relaxed)
}

/// The spinner frame for now.
pub fn frame() -> &'static str {
    frame_of(STYLES[STYLE.load(Ordering::Relaxed) as usize % STYLES.len()].0)
}

/// A style's frame for now (also for the settings preview).
pub fn frame_of(style: &str) -> &'static str {
    let frames = STYLES
        .iter()
        .find(|(n, _)| *n == style)
        .map(|(_, f)| *f)
        .unwrap_or(STYLES[0].1);
    let every = if LOW_POWER.load(Ordering::Relaxed) {
        250
    } else {
        // the circle turns at half speed: four big frames look calmer
        if frames.len() <= 4 { 200 } else { 100 }
    };
    let t = (epoch().elapsed().as_millis() / every) as usize;
    frames[t % frames.len()]
}

const EIGHTHS: [&str; 8] = ["", "▏", "▎", "▍", "▌", "▋", "▊", "▉"];

/// A filling bar `width` cells wide: "████▌   ". Without a percentage, a
/// short block that slides back and forth.
pub fn bar(progress: Option<u8>, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    match progress {
        Some(p) => {
            let eighths = (p.min(100) as usize * width * 8) / 100;
            let full = eighths / 8;
            let mut s = "█".repeat(full);
            if full < width {
                s.push_str(EIGHTHS[eighths % 8]);
                let used = full + usize::from(eighths % 8 > 0);
                s.push_str(&" ".repeat(width - used));
            }
            s
        }
        None => {
            let block = (width / 4).max(1);
            let span = width - block;
            let t = (epoch().elapsed().as_millis() / 120) as usize;
            let pos = if span == 0 {
                0
            } else {
                let k = t % (2 * span);
                if k < span { k } else { 2 * span - k }
            };
            format!(
                "{}{}{}",
                " ".repeat(pos),
                "█".repeat(block),
                " ".repeat(width - pos - block)
            )
        }
    }
}

/// "12s", "3m 05s", "1h 02m".
pub fn eta(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m {:02}s", secs / 60, secs % 60)
    } else {
        format!("{}h {:02}m", secs / 3600, (secs % 3600) / 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bars_fill() {
        assert_eq!(bar(Some(0), 4), "    ");
        assert_eq!(bar(Some(50), 4), "██  ");
        assert_eq!(bar(Some(100), 4), "████");
        assert_eq!(bar(Some(45), 4), "█▊  ");
        for p in 0..=100 {
            assert_eq!(bar(Some(p), 10).chars().count(), 10, "{p}");
        }
        let b = bar(None, 10);
        assert_eq!(b.chars().count(), 10);
        assert!(b.contains('█'));
        assert_eq!(eta(7), "7s");
        assert_eq!(eta(185), "3m 05s");
        assert_eq!(eta(3720), "1h 02m");
    }

    #[test]
    fn styles() {
        for (name, frames) in STYLES {
            assert!(frames.contains(&frame_of(name)), "{name}");
        }
        assert!(STYLES[0].1.contains(&frame_of("nope")));
    }
}
