//! Keyboard input: split raw terminal bytes into keys and map Shellvisor's own
//! shortcuts. Everything that isn't a Shellvisor shortcut is forwarded to the
//! focused shell byte-for-byte.
//!
//! Shortcuts are written as text (`alt+t`, `ctrl+]`, `f7`, `alt+shift+left`)
//! both in the defaults below and in `config.yaml`, and parsed into the byte
//! sequences terminals actually send.

use std::collections::HashMap;

/// Split one read from stdin into individual key sequences.
///
/// A bracketed paste (`ESC[200~ … ESC[201~`) is always one token, so pasted
/// text is never interpreted as shortcuts.
pub fn tokenize(chunk: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < chunk.len() {
        let len = token_len(&chunk[i..]);
        out.push(&chunk[i..i + len]);
        i += len;
    }
    out
}

const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn token_len(s: &[u8]) -> usize {
    if s.starts_with(PASTE_START) {
        return find(s, PASTE_END)
            .map(|p| p + PASTE_END.len())
            .unwrap_or(s.len());
    }
    match s {
        [0x1b] => 1,
        [0x1b, b'[', rest @ ..] => {
            // X10 mouse: ESC [ M b x y
            if rest.first() == Some(&b'M') && rest.len() >= 4 {
                return 6;
            }
            // CSI: parameters/intermediates until a final byte 0x40..=0x7E
            match rest.iter().position(|&b| (0x40..=0x7e).contains(&b)) {
                Some(p) => 2 + p + 1,
                None => s.len(),
            }
        }
        [0x1b, b'O', _, ..] => 3,
        [0x1b, 0x1b, ..] => 1 + token_len(&s[1..]), // alt + escape sequence
        [0x1b, b, ..] => 1 + utf8_len(*b).min(s.len() - 1), // alt + char
        [b, ..] => utf8_len(*b).min(s.len()),
        [] => 0,
    }
}

fn utf8_len(b: u8) -> usize {
    match b {
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => 1,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

/// Shellvisor commands a key can trigger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Cmd {
    Quit,
    ToggleView,
    Help,
    Leader,
    NewTab,
    CloseTab,
    NextTab,
    PrevTab,
    GoTab(usize),
    RenameTab,
    SplitRight,
    SplitDown,
    ClosePane,
    Focus(Dir),
    NextPane,
    Resize(Dir),
    Zoom,
    ScrollUp,
    ScrollDown,
    NextLayout,
    TabMenu,
    ToggleFiles,
    ToggleSystem,
    LowPower,
    Preset(Preset),
}

/// Ready-made layouts for a tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Preset {
    /// Shell on the left; files above system on the right.
    Dev,
    /// System stats big, shell below.
    Monitor,
    /// Files on the left, shell on the right.
    Files,
}

impl Preset {
    pub const ALL: [Preset; 3] = [Preset::Dev, Preset::Monitor, Preset::Files];
    pub fn name(self) -> &'static str {
        match self {
            Preset::Dev => "dev",
            Preset::Monitor => "monitor",
            Preset::Files => "files",
        }
    }
    pub fn parse(s: &str) -> Option<Preset> {
        Preset::ALL.into_iter().find(|p| p.name() == s)
    }
}

/// Every bindable action: config name, command, default keys, help text.
pub const ACTIONS: &[(&str, Cmd, &[&str], &str)] = &[
    ("help", Cmd::Help, &["f1"], "this help"),
    (
        "toggle_view",
        Cmd::ToggleView,
        &["alt+`"],
        "full view ⇄ basic view (also: sv on / sv off)",
    ),
    (
        "quit",
        Cmd::Quit,
        &["ctrl+q"],
        "quit (asks if commands are running)",
    ),
    (
        "leader",
        Cmd::Leader,
        &["ctrl+]"],
        "leader key: then one letter (works in every terminal)",
    ),
    ("tab_menu", Cmd::TabMenu, &["alt+m"], "tab menu"),
    (
        "new_tab",
        Cmd::NewTab,
        &["alt+t", "f10"],
        "new tab in the same folder",
    ),
    ("close_tab", Cmd::CloseTab, &["alt+w"], "close tab"),
    ("rename_tab", Cmd::RenameTab, &["alt+r"], "rename tab"),
    ("next_tab", Cmd::NextTab, &["alt+]"], "next tab"),
    ("prev_tab", Cmd::PrevTab, &["alt+["], "previous tab"),
    ("go_tab_1", Cmd::GoTab(0), &["alt+1"], "go to tab 1 (…9)"),
    ("go_tab_2", Cmd::GoTab(1), &["alt+2"], ""),
    ("go_tab_3", Cmd::GoTab(2), &["alt+3"], ""),
    ("go_tab_4", Cmd::GoTab(3), &["alt+4"], ""),
    ("go_tab_5", Cmd::GoTab(4), &["alt+5"], ""),
    ("go_tab_6", Cmd::GoTab(5), &["alt+6"], ""),
    ("go_tab_7", Cmd::GoTab(6), &["alt+7"], ""),
    ("go_tab_8", Cmd::GoTab(7), &["alt+8"], ""),
    ("go_tab_9", Cmd::GoTab(8), &["alt+9"], ""),
    (
        "split_right",
        Cmd::SplitRight,
        &["alt+\\"],
        "split side by side",
    ),
    ("split_down", Cmd::SplitDown, &["alt+-"], "split stacked"),
    (
        "close_pane",
        Cmd::ClosePane,
        &[],
        "close split (also: ctrl+] x)",
    ),
    (
        "next_layout",
        Cmd::NextLayout,
        &["f7", "alt+l"],
        "rearrange splits: columns · rows · main+side · grid",
    ),
    (
        "focus_left",
        Cmd::Focus(Dir::Left),
        &["alt+left"],
        "move to the split on the left (arrows)",
    ),
    ("focus_right", Cmd::Focus(Dir::Right), &["alt+right"], ""),
    ("focus_up", Cmd::Focus(Dir::Up), &["alt+up"], ""),
    ("focus_down", Cmd::Focus(Dir::Down), &["alt+down"], ""),
    ("next_pane", Cmd::NextPane, &["f6"], "next split"),
    (
        "resize_left",
        Cmd::Resize(Dir::Left),
        &["alt+shift+left"],
        "resize split (arrows)",
    ),
    (
        "resize_right",
        Cmd::Resize(Dir::Right),
        &["alt+shift+right"],
        "",
    ),
    ("resize_up", Cmd::Resize(Dir::Up), &["alt+shift+up"], ""),
    (
        "resize_down",
        Cmd::Resize(Dir::Down),
        &["alt+shift+down"],
        "",
    ),
    ("zoom", Cmd::Zoom, &["f5"], "zoom split full-screen"),
    (
        "scroll_up",
        Cmd::ScrollUp,
        &["shift+pageup"],
        "scroll back one page (or mouse wheel)",
    ),
    (
        "scroll_down",
        Cmd::ScrollDown,
        &["shift+pagedown"],
        "scroll forward one page",
    ),
    (
        "toggle_files",
        Cmd::ToggleFiles,
        &["f2"],
        "files pane: open · focus · close (also: sv files)",
    ),
    (
        "toggle_system",
        Cmd::ToggleSystem,
        &["f3"],
        "system pane: open · focus · close (also: sv sys)",
    ),
    (
        "preset_dev",
        Cmd::Preset(Preset::Dev),
        &[],
        "layouts: dev · monitor · files (menu, or sv layout <name>)",
    ),
    ("preset_monitor", Cmd::Preset(Preset::Monitor), &[], ""),
    ("preset_files", Cmd::Preset(Preset::Files), &[], ""),
    (
        "low_power",
        Cmd::LowPower,
        &[],
        "low-power mode on/off (also: sv power on|off|auto)",
    ),
];

/// Turn `alt+t`, `ctrl+]`, `f7`, `alt+shift+left` … into the byte sequences
/// terminals send for it (some keys have more than one).
pub fn parse_spec(spec: &str) -> Result<Vec<Vec<u8>>, String> {
    let s = spec.trim().to_lowercase();
    let (mods, key) = match s.rsplit_once('+') {
        // "alt++" would be ambiguous; "+" alone isn't a useful shortcut
        Some((m, k)) if !k.is_empty() => (m, k),
        _ => ("", s.as_str()),
    };
    let (mut ctrl, mut alt, mut shift) = (false, false, false);
    for m in mods.split('+').filter(|m| !m.is_empty()) {
        match m {
            "ctrl" | "control" | "c" => ctrl = true,
            "alt" | "opt" | "option" | "meta" | "m" => alt = true,
            "shift" | "s" => shift = true,
            other => return Err(format!("`{spec}`: unknown modifier `{other}`")),
        }
    }
    let m = 1 + shift as u8 + 2 * alt as u8 + 4 * ctrl as u8;
    let csi_mod = |base: &str, fin: char| format!("\x1b[{base};{m}{fin}").into_bytes();
    let blocks_typing = || {
        Err(format!(
            "`{spec}` has no ctrl/alt — it would stop you typing it"
        ))
    };

    // Function keys
    if let Some(n) = key.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()) {
        let tilde = |n: u8| -> Option<u8> {
            Some(match n {
                5 => 15,
                6 => 17,
                7 => 18,
                8 => 19,
                9 => 20,
                10 => 21,
                11 => 23,
                12 => 24,
                _ => return None,
            })
        };
        return match (n, m) {
            (1..=4, 1) => {
                let c = (b'P' + n - 1) as char;
                Ok(vec![
                    format!("\x1bO{c}").into_bytes(),
                    format!("\x1b[{}~", 10 + n).into_bytes(),
                ])
            }
            (1..=4, _) => Ok(vec![csi_mod("1", (b'P' + n - 1) as char)]),
            (5..=12, 1) => Ok(vec![format!("\x1b[{}~", tilde(n).unwrap()).into_bytes()]),
            (5..=12, _) => Ok(vec![
                format!("\x1b[{};{m}~", tilde(n).unwrap()).into_bytes(),
            ]),
            _ => Err(format!("`{spec}`: no such function key")),
        };
    }

    let arrow = match key {
        "up" => Some('A'),
        "down" => Some('B'),
        "right" => Some('C'),
        "left" => Some('D'),
        _ => None,
    };
    if let Some(a) = arrow {
        if m == 1 {
            return blocks_typing();
        }
        let mut v = vec![csi_mod("1", a)];
        if m == 3 {
            v.push(format!("\x1b\x1b[{a}").into_bytes()); // alt+arrow in Terminal.app
        }
        return Ok(v);
    }
    match key {
        "pageup" | "pgup" | "pagedown" | "pgdn" => {
            if m == 1 {
                return blocks_typing();
            }
            let n = if key.starts_with("pageu") || key == "pgup" {
                "5"
            } else {
                "6"
            };
            return Ok(vec![format!("\x1b[{n};{m}~").into_bytes()]);
        }
        "space" | "enter" | "tab" | "esc" | "backspace" => {
            let b = match key {
                "space" => b' ',
                "enter" => b'\r',
                "tab" => b'\t',
                "esc" => 0x1b,
                _ => 0x7f,
            };
            return match (ctrl, alt) {
                (true, false) if key == "space" => Ok(vec![vec![0x00]]),
                (false, true) => Ok(vec![vec![0x1b, b]]),
                _ => Err(format!("`{spec}`: use alt+{key}")),
            };
        }
        _ => {}
    }

    let mut chars = key.chars();
    let (Some(c), None) = (chars.next(), chars.next()) else {
        return Err(format!("`{spec}`: unknown key `{key}`"));
    };
    if !c.is_ascii() || c.is_ascii_control() {
        return Err(format!("`{spec}`: unsupported key `{key}`"));
    }
    let c = c as u8;
    let ctrl_byte = |c: u8| -> Option<u8> {
        match c {
            b'a'..=b'z' => Some(c - b'a' + 1),
            b'@' | b'2' | b' ' => Some(0x00),
            b'\\' => Some(0x1c),
            b']' => Some(0x1d),
            b'^' | b'6' => Some(0x1e),
            b'_' | b'-' | b'/' => Some(0x1f),
            _ => None,
        }
    };
    let ch = if shift && c.is_ascii_lowercase() {
        c.to_ascii_uppercase()
    } else {
        c
    };
    match (ctrl, alt) {
        (false, false) => blocks_typing(),
        (false, true) => Ok(vec![vec![0x1b, ch]]),
        (true, alt) => {
            let b = ctrl_byte(c)
                .ok_or_else(|| format!("`{spec}`: terminals can't send ctrl+{}", c as char))?;
            Ok(vec![if alt { vec![0x1b, b] } else { vec![b] }])
        }
    }
}

/// The active shortcuts: defaults, with `config.yaml` overrides applied.
#[derive(Debug, Clone)]
pub struct Keymap {
    bindings: Vec<(Vec<u8>, Cmd)>,
    shown: HashMap<&'static str, String>,
}

impl Default for Keymap {
    fn default() -> Self {
        Keymap::new(&HashMap::new()).0
    }
}

impl Keymap {
    /// `overrides`: action name → key specs (`["none"]` or `[]` unbinds).
    /// Returns the keymap and any problems to tell the user about.
    pub fn new(overrides: &HashMap<String, Vec<String>>) -> (Keymap, Vec<String>) {
        let mut warnings = Vec::new();
        for name in overrides.keys() {
            if !ACTIONS.iter().any(|(n, ..)| n == name) {
                warnings.push(format!("keys: unknown action `{name}`"));
            }
        }
        let mut bindings: Vec<(Vec<u8>, Cmd)> = Vec::new();
        let mut shown = HashMap::new();
        for (name, cmd, defaults, _) in ACTIONS {
            let specs: Vec<String> = match overrides.get(*name) {
                Some(v) => v.clone(),
                None => defaults.iter().map(|s| s.to_string()).collect(),
            };
            let mut labels = Vec::new();
            for spec in specs.iter().filter(|s| !s.eq_ignore_ascii_case("none")) {
                match parse_spec(spec) {
                    Ok(seqs) => {
                        for seq in seqs {
                            if let Some((_, other)) = bindings.iter().find(|(s, _)| *s == seq) {
                                warnings.push(format!(
                                    "keys: `{spec}` is used for two actions ({other:?} and {name})"
                                ));
                            }
                            bindings.push((seq, *cmd));
                        }
                        labels.push(spec.to_lowercase());
                    }
                    Err(e) => warnings.push(format!("keys.{name}: {e}")),
                }
            }
            shown.insert(*name, labels.join(" · "));
        }
        (Keymap { bindings, shown }, warnings)
    }

    /// The command bound to this key, if any. First binding wins.
    pub fn lookup(&self, key: &[u8]) -> Option<Cmd> {
        self.bindings
            .iter()
            .find(|(s, _)| s == key)
            .map(|(_, c)| *c)
    }

    /// The keys bound to an action, for the help screen ("" if unbound).
    pub fn shown(&self, action: &str) -> &str {
        self.shown.get(action).map(String::as_str).unwrap_or("")
    }
}

/// The key pressed after the leader (ctrl+]).
pub fn after_leader(key: &[u8]) -> Option<Cmd> {
    use Cmd::*;
    Some(match key {
        b"t" | b"c" => NewTab,
        b"w" => CloseTab,
        b"x" => ClosePane,
        b"n" => NextTab,
        b"p" => PrevTab,
        b"r" => RenameTab,
        b"\\" | b"|" => SplitRight,
        b"-" => SplitDown,
        b"o" => NextPane,
        b"z" => Zoom,
        b"l" => NextLayout,
        b"m" => TabMenu,
        b"b" => ToggleView,
        b"?" | b"h" => Help,
        b"q" => Quit,
        b"f" => ToggleFiles,
        b"s" => ToggleSystem,
        b"\x1b[A" => Focus(Dir::Up),
        b"\x1b[B" => Focus(Dir::Down),
        b"\x1b[C" => Focus(Dir::Right),
        b"\x1b[D" => Focus(Dir::Left),
        [d @ b'1'..=b'9'] => GoTab((d - b'1') as usize),
        _ => return None,
    })
}

pub const LEADER_HINT: &str = " ctrl+] → t tab · w close tab · x close split · \\ split │ · - split ─ · o next split · z zoom · l layout · m menu · f files · s system · b basic view · r rename · 1-9 tab · ? help · ctrl+] send ctrl+] ";

/// On macOS, Option+key types these characters unless the terminal is set to
/// send Option as Alt/Meta. Seeing one alone means alt shortcuts can't work.
pub fn is_option_char(key: &[u8]) -> bool {
    const CHARS: [&str; 16] = [
        "¡", "™", "£", "¢", "∞", "§", "¶", "•", "ª", // option+1…9
        "†", // option+t
        "∑", // option+w
        "«", // option+\
        "–", // option+-
        "‘", // option+]
        "¬", // option+l
        "µ", // option+m
    ];
    std::str::from_utf8(key).is_ok_and(|s| CHARS.contains(&s))
}

/// How to switch Option to Alt in the user's terminal.
pub fn option_fix_hint() -> &'static str {
    match std::env::var("TERM_PROGRAM").as_deref().unwrap_or("") {
        "Apple_Terminal" => {
            "Terminal → Settings → Profiles → Keyboard → tick “Use Option as Meta key”"
        }
        "iTerm.app" => "iTerm2 → Settings → Profiles → Keys → Left Option key → Esc+",
        "ghostty" => "Ghostty config: macos-option-as-alt = true",
        _ => "set your terminal to send Option as Alt/Meta — or use the leader key ctrl+]",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizes_mixed_input() {
        let t = tokenize(b"ab\x1b[A\x1bOP\x1bt\x1b\x1b[D\x11");
        assert_eq!(
            t,
            vec![
                &b"a"[..],
                b"b",
                b"\x1b[A",
                b"\x1bOP",
                b"\x1bt",
                b"\x1b\x1b[D",
                b"\x11"
            ]
        );
    }

    #[test]
    fn utf8_and_paste_stay_whole() {
        assert_eq!(
            tokenize("é界".as_bytes()),
            vec!["é".as_bytes(), "界".as_bytes()]
        );
        let p = b"\x1b[200~\x1bt\x11 hi\x1b[201~x";
        assert_eq!(tokenize(p), vec![&p[..p.len() - 1], b"x"]);
    }

    #[test]
    fn mouse_sequences_are_single_tokens() {
        assert_eq!(
            tokenize(b"\x1b[<0;5;3M\x1b[<0;5;3m"),
            vec![&b"\x1b[<0;5;3M"[..], b"\x1b[<0;5;3m"]
        );
        assert_eq!(tokenize(b"\x1b[M !!q"), vec![&b"\x1b[M !!"[..], b"q"]);
    }

    #[test]
    fn specs_parse_to_terminal_bytes() {
        let one = |s| parse_spec(s).unwrap();
        assert_eq!(one("alt+t"), vec![b"\x1bt".to_vec()]);
        assert_eq!(one("Alt+T"), vec![b"\x1bt".to_vec()]);
        assert_eq!(one("alt+shift+t"), vec![b"\x1bT".to_vec()]);
        assert_eq!(one("ctrl+q"), vec![vec![0x11]]);
        assert_eq!(one("ctrl+]"), vec![vec![0x1d]]);
        assert_eq!(one("ctrl+space"), vec![vec![0x00]]);
        assert_eq!(one("alt+`"), vec![b"\x1b`".to_vec()]);
        assert_eq!(one("alt+\\"), vec![b"\x1b\\".to_vec()]);
        assert_eq!(one("alt+-"), vec![b"\x1b-".to_vec()]);
        assert_eq!(one("f1"), vec![b"\x1bOP".to_vec(), b"\x1b[11~".to_vec()]);
        assert_eq!(one("f7"), vec![b"\x1b[18~".to_vec()]);
        assert_eq!(one("shift+f7"), vec![b"\x1b[18;2~".to_vec()]);
        assert_eq!(
            one("alt+left"),
            vec![b"\x1b[1;3D".to_vec(), b"\x1b\x1b[D".to_vec()]
        );
        assert_eq!(one("alt+shift+down"), vec![b"\x1b[1;4B".to_vec()]);
        assert_eq!(one("shift+pageup"), vec![b"\x1b[5;2~".to_vec()]);
    }

    #[test]
    fn bad_specs_are_rejected() {
        for bad in [
            "t", "left", "pageup", "hyper+t", "ctrl+1", "f13", "alt+tea", "alt+é",
        ] {
            assert!(parse_spec(bad).is_err(), "{bad} should be rejected");
        }
    }

    #[test]
    fn default_keymap_matches_the_documented_keys() {
        let k = Keymap::default();
        assert_eq!(k.lookup(b"\x1bt"), Some(Cmd::NewTab));
        assert_eq!(k.lookup(b"\x1b[21~"), Some(Cmd::NewTab));
        assert_eq!(k.lookup(b"\x1b3"), Some(Cmd::GoTab(2)));
        assert_eq!(k.lookup(b"\x1b[1;3C"), Some(Cmd::Focus(Dir::Right)));
        assert_eq!(k.lookup(b"\x1b\x1b[A"), Some(Cmd::Focus(Dir::Up)));
        assert_eq!(k.lookup(b"\x1b[1;4B"), Some(Cmd::Resize(Dir::Down)));
        assert_eq!(k.lookup(b"\x1b`"), Some(Cmd::ToggleView));
        assert_eq!(k.lookup(b"\x1b[18~"), Some(Cmd::NextLayout));
        assert_eq!(k.lookup(b"\x1bm"), Some(Cmd::TabMenu));
        assert_eq!(k.lookup(&[0x11]), Some(Cmd::Quit));
        assert_eq!(k.lookup(&[0x1d]), Some(Cmd::Leader));
        // ordinary keys are not ours
        for key in [
            &b"a"[..],
            b"\x1b[A",
            b"\x1bb",
            b"\x1bf",
            b"\x1b.",
            b"\x12",
            b"\x10",
        ] {
            assert_eq!(k.lookup(key), None, "{key:?}");
        }
        assert_eq!(k.shown("new_tab"), "alt+t · f10");
        assert_eq!(k.shown("close_pane"), "");
    }

    #[test]
    fn overrides_replace_and_unbind() {
        let mut o = HashMap::new();
        o.insert("new_tab".to_string(), vec!["alt+n".to_string()]);
        o.insert("next_layout".to_string(), vec!["none".to_string()]);
        o.insert("bogus".to_string(), vec!["alt+z".to_string()]);
        o.insert("zoom".to_string(), vec!["z".to_string()]);
        let (k, warn) = Keymap::new(&o);
        assert_eq!(k.lookup(b"\x1bn"), Some(Cmd::NewTab));
        assert_eq!(k.lookup(b"\x1bt"), None, "alt+t is free for the shell now");
        assert_eq!(k.lookup(b"\x1bl"), None);
        assert_eq!(k.lookup(b"\x1b[18~"), None);
        assert_eq!(
            k.lookup(b"\x1b[15~"),
            None,
            "zoom's bad override unbinds f5"
        );
        assert!(warn.iter().any(|w| w.contains("bogus")), "{warn:?}");
        assert!(warn.iter().any(|w| w.contains("keys.zoom")), "{warn:?}");
    }

    #[test]
    fn duplicate_bindings_are_reported() {
        let mut o = HashMap::new();
        o.insert("zoom".to_string(), vec!["alt+t".to_string()]);
        let (_, warn) = Keymap::new(&o);
        assert!(warn.iter().any(|w| w.contains("two actions")), "{warn:?}");
    }

    #[test]
    fn leader_keys() {
        assert_eq!(after_leader(b"-"), Some(Cmd::SplitDown));
        assert_eq!(after_leader(b"2"), Some(Cmd::GoTab(1)));
        assert_eq!(after_leader(b"\x1b[C"), Some(Cmd::Focus(Dir::Right)));
        assert_eq!(after_leader(b"l"), Some(Cmd::NextLayout));
        assert_eq!(after_leader(b"k"), None);
    }

    #[test]
    fn option_chars() {
        assert!(is_option_char("¡".as_bytes()));
        assert!(is_option_char("†".as_bytes()));
        assert!(!is_option_char(b"a"));
    }
}
