//! `sv autostart on|off|status`: start Shellvisor when a terminal opens.
//!
//! Adds a marked block to your shell's startup file (~/.zshrc, or ~/.bashrc
//! and ~/.bash_profile for bash). It runs Shellvisor only in an interactive
//! terminal that isn't already inside Shellvisor, tmux, screen, an editor's
//! terminal or an SSH session, and not when `SHELLVISOR_NO_AUTOSTART` is set.
//! Quitting Shellvisor leaves you in your plain shell (nothing is `exec`ed),
//! so a broken setup can never lock you out. `off` removes the block.

use std::path::{Path, PathBuf};

const BEGIN: &str = "# >>> shellvisor autostart >>>";
const END: &str = "# <<< shellvisor autostart <<<";

fn block() -> String {
    format!(
        "{BEGIN}
# Added by `sv autostart on`; remove with `sv autostart off`.
if [ -z \"$SHELLVISOR\" ] && [ -z \"$SHELLVISOR_NO_AUTOSTART\" ] && [ -t 0 ] && [ -t 1 ] \\
   && [ -z \"$TMUX\" ] && [ -z \"$STY\" ] && [ -z \"$INSIDE_EMACS\" ] && [ -z \"$SSH_TTY\" ] \\
   && [ \"$TERM_PROGRAM\" != \"vscode\" ] && [ \"$TERM\" != \"dumb\" ] \\
   && command -v shellvisor >/dev/null 2>&1; then
  shellvisor
fi
{END}
"
    )
}

/// The startup files for this shell (`zsh` or `bash`, from a name or path).
pub fn rc_files(home: &Path, shell: &str) -> Vec<PathBuf> {
    let name = shell.rsplit('/').next().unwrap_or(shell);
    if name.contains("zsh") {
        let dir = std::env::var_os("ZDOTDIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.to_path_buf());
        vec![dir.join(".zshrc")]
    } else if name.contains("bash") {
        // login shells (macOS Terminal) read .bash_profile; others .bashrc
        let mut v = vec![home.join(".bashrc")];
        let profile = home.join(".bash_profile");
        if profile.exists() {
            v.push(profile);
        }
        v
    } else {
        Vec::new()
    }
}

pub fn installed(text: &str) -> bool {
    text.contains(BEGIN)
}

/// The file's text with the block added (once, at the end).
pub fn add(text: &str) -> String {
    if installed(text) {
        return text.to_string();
    }
    let mut out = text.to_string();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    if !out.is_empty() {
        out.push('\n');
    }
    out + &block()
}

/// The file's text without the block (and the blank line before it).
pub fn remove(text: &str) -> String {
    let (Some(b), Some(e)) = (text.find(BEGIN), text.find(END)) else {
        return text.to_string();
    };
    if e < b {
        return text.to_string();
    }
    let mut end = e + END.len();
    if text[end..].starts_with('\n') {
        end += 1;
    }
    let mut start = b;
    if text[..start].ends_with("\n\n") {
        start -= 1;
    }
    format!("{}{}", &text[..start], &text[end..])
}

/// `sv autostart …`
pub fn run(args: &[String]) -> i32 {
    let home = PathBuf::from(std::env::var("HOME").unwrap_or_default());
    let shell = std::env::var("SHELL").unwrap_or_default();
    let files = rc_files(&home, &shell);
    if files.is_empty() {
        eprintln!("sv autostart: works with zsh and bash (your $SHELL is `{shell}`)");
        return 1;
    }
    match args.first().map(String::as_str) {
        Some("on") => {
            for f in &files {
                let text = std::fs::read_to_string(f).unwrap_or_default();
                if installed(&text) {
                    println!("already on in {}", f.display());
                    continue;
                }
                if let Err(e) = std::fs::write(f, add(&text)) {
                    eprintln!("sv autostart: {}: {e}", f.display());
                    return 1;
                }
                println!("on: new terminals start Shellvisor ({})", f.display());
            }
            println!(
                "skip it once with SHELLVISOR_NO_AUTOSTART=1; turn it off with `sv autostart off`"
            );
            0
        }
        Some("off") => {
            for f in &files {
                let Ok(text) = std::fs::read_to_string(f) else {
                    continue;
                };
                if installed(&text) {
                    if let Err(e) = std::fs::write(f, remove(&text)) {
                        eprintln!("sv autostart: {}: {e}", f.display());
                        return 1;
                    }
                    println!("off: removed from {}", f.display());
                }
            }
            0
        }
        None | Some("status") => {
            let on: Vec<String> = files
                .iter()
                .filter(|f| std::fs::read_to_string(f).is_ok_and(|t| installed(&t)))
                .map(|f| f.display().to_string())
                .collect();
            if on.is_empty() {
                println!("autostart is off (sv autostart on)");
            } else {
                println!("autostart is on ({})", on.join(", "));
            }
            0
        }
        Some(other) => {
            eprintln!("sv autostart: `{other}` — on, off or status");
            2
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_and_remove_leave_the_file_as_it_was() {
        let rc = "export PATH=$HOME/bin:$PATH\nalias ll='ls -l'\n";
        let on = add(rc);
        assert!(installed(&on) && on.starts_with(rc));
        assert!(on.contains("[ -z \"$SHELLVISOR\" ]") && on.contains("  shellvisor\n"));
        assert!(
            !on.contains("exec "),
            "never exec: quitting must leave a shell"
        );
        assert_eq!(add(&on), on, "only once");
        assert_eq!(remove(&on), rc);
        assert_eq!(remove(rc), rc);
        assert_eq!(remove(&add("")), "");
        let no_newline = "alias x=y";
        assert_eq!(remove(&add(no_newline)), "alias x=y\n");
    }

    #[test]
    fn which_files() {
        let home = std::env::temp_dir().join(format!("sv-autostart-{}", std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        unsafe { std::env::remove_var("ZDOTDIR") };
        assert_eq!(rc_files(&home, "/bin/zsh"), vec![home.join(".zshrc")]);
        assert_eq!(rc_files(&home, "bash"), vec![home.join(".bashrc")]);
        std::fs::write(home.join(".bash_profile"), "").unwrap();
        assert_eq!(rc_files(&home, "/usr/local/bin/bash").len(), 2);
        assert!(rc_files(&home, "/usr/bin/fish").is_empty());
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn the_block_is_valid_sh() {
        let out = std::process::Command::new("sh")
            .arg("-n")
            .arg("-c")
            .arg(block())
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
