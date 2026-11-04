//! Tiny system helpers for the status bar (no extra dependencies).

/// Shellvisor's own memory use, e.g. "4.1M".
/// Linux: current resident size. macOS: peak resident size (no cheap
/// "current" API without extra bindings) — an upper bound, which is what the
/// budget cares about.
pub fn self_memory() -> String {
    format!("{:.1}M", self_memory_bytes() as f64 / 1_048_576.0)
}

pub fn self_memory_bytes() -> u64 {
    #[cfg(target_os = "linux")]
    {
        if let Ok(s) = std::fs::read_to_string("/proc/self/statm") {
            if let Some(pages) = s
                .split_whitespace()
                .nth(1)
                .and_then(|v| v.parse::<u64>().ok())
            {
                let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as u64;
                return pages * page;
            }
        }
    }
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut ru) } != 0 {
        return 0;
    }
    let max = ru.ru_maxrss as u64;
    // ru_maxrss is bytes on macOS, kilobytes elsewhere.
    if cfg!(target_os = "macos") {
        max
    } else {
        max * 1024
    }
}

/// Local time as "HH:MM".
pub fn clock() -> String {
    unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&t, &mut tm).is_null() {
            return String::new();
        }
        format!("{:02}:{:02}", tm.tm_hour, tm.tm_min)
    }
}

/// Running on battery power? None if we can't tell (desktops, unknown OS).
pub fn on_battery() -> Option<bool> {
    #[cfg(target_os = "linux")]
    {
        let dir = std::fs::read_dir("/sys/class/power_supply").ok()?;
        let mut mains = None;
        for e in dir.flatten() {
            let p = e.path();
            let kind = std::fs::read_to_string(p.join("type")).unwrap_or_default();
            if kind.trim() == "Mains" {
                let online = std::fs::read_to_string(p.join("online")).unwrap_or_default();
                let on = online.trim() == "1";
                mains = Some(mains.unwrap_or(false) || on);
            }
        }
        mains.map(|on_ac| !on_ac)
    }
    #[cfg(target_os = "macos")]
    {
        let out = std::process::Command::new("pmset")
            .args(["-g", "ps"])
            .stderr(std::process::Stdio::null())
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        if text.contains("'Battery Power'") {
            Some(true)
        } else if text.contains("'AC Power'") {
            Some(false)
        } else {
            None
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn values_look_sane() {
        let b = super::self_memory_bytes();
        assert!(b > 100_000 && b < 4_000_000_000, "{b}");
        let c = super::clock();
        assert_eq!(c.len(), 5);
        assert_eq!(&c[2..3], ":");
    }
}
