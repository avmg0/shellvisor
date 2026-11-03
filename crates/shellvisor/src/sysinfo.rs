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
