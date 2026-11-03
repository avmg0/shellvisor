//! Keyboard input: split raw terminal bytes into keys and map Shellvisor's own
//! shortcuts. Everything that isn't a Shellvisor shortcut is forwarded to the
//! focused shell byte-for-byte.

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

/// Shellvisor commands a key can trigger.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
}

fn arrow(b: u8) -> Option<Dir> {
    match b {
        b'A' => Some(Dir::Up),
        b'B' => Some(Dir::Down),
        b'C' => Some(Dir::Right),
        b'D' => Some(Dir::Left),
        _ => None,
    }
}

/// Shortcuts that work anywhere (also inside vim, less, …).
pub fn global(key: &[u8]) -> Option<Cmd> {
    use Cmd::*;
    Some(match key {
        [0x11] => Quit,   // ctrl+q
        [0x1d] => Leader, // ctrl+]
        b"\x1b`" => ToggleView,
        b"\x1bOP" | b"\x1b[11~" => Help, // F1
        b"\x1b[15~" => Zoom,             // F5
        b"\x1b[17~" => NextPane,         // F6
        b"\x1b[21~" => NewTab,           // F10
        b"\x1bt" => NewTab,
        b"\x1bw" => CloseTab,
        b"\x1br" => RenameTab,
        b"\x1b[" => PrevTab, // alt+[ (only when it arrives on its own)
        b"\x1b]" => NextTab,
        b"\x1b\\" => SplitRight,
        b"\x1b-" => SplitDown,
        b"\x1b[5;2~" => ScrollUp,   // shift+PageUp
        b"\x1b[6;2~" => ScrollDown, // shift+PageDown
        [0x1b, d @ b'1'..=b'9'] => GoTab((d - b'1') as usize),
        // alt+arrows: xterm/iTerm2 style, and Terminal.app style (ESC ESC [ A)
        [0x1b, b'[', b'1', b';', b'3', a] => Focus(arrow(*a)?),
        [0x1b, 0x1b, b'[', a] => Focus(arrow(*a)?),
        // alt+shift+arrows
        [0x1b, b'[', b'1', b';', b'4', a] => Resize(arrow(*a)?),
        _ => return None,
    })
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
        b"b" => ToggleView,
        b"?" | b"h" => Help,
        b"q" => Quit,
        b"\x1b[A" => Focus(Dir::Up),
        b"\x1b[B" => Focus(Dir::Down),
        b"\x1b[C" => Focus(Dir::Right),
        b"\x1b[D" => Focus(Dir::Left),
        [d @ b'1'..=b'9'] => GoTab((d - b'1') as usize),
        _ => return None,
    })
}

pub const LEADER_HINT: &str = " ctrl+] → t tab · w close tab · x close pane · \\ split │ · - split ─ · o next pane · z zoom · b basic view · r rename · 1-9 tab · ? help · ctrl+] send ctrl+] ";

/// On macOS, Option+key types these characters unless the terminal is set to
/// send Option as Alt/Meta. Seeing one alone means alt shortcuts can't work.
pub fn is_option_char(key: &[u8]) -> bool {
    const CHARS: [&str; 14] = [
        "¡", "™", "£", "¢", "∞", "§", "¶", "•", "ª", // option+1…9
        "†", // option+t
        "∑", // option+w
        "«", // option+\
        "–", // option+-
        "‘", // option+]
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
    fn global_keys() {
        assert_eq!(global(b"\x1bt"), Some(Cmd::NewTab));
        assert_eq!(global(b"\x1b3"), Some(Cmd::GoTab(2)));
        assert_eq!(global(b"\x1b[1;3C"), Some(Cmd::Focus(Dir::Right)));
        assert_eq!(global(b"\x1b\x1b[A"), Some(Cmd::Focus(Dir::Up)));
        assert_eq!(global(b"\x1b[1;4B"), Some(Cmd::Resize(Dir::Down)));
        assert_eq!(global(b"\x1b[21~"), Some(Cmd::NewTab));
        assert_eq!(global(b"\x1b`"), Some(Cmd::ToggleView));
        // ordinary keys are not ours
        for k in [
            &b"a"[..],
            b"\x1b[A",
            b"\x1bb",
            b"\x1bf",
            b"\x1b.",
            b"\x12",
            b"\x10",
        ] {
            assert_eq!(global(k), None, "{k:?}");
        }
    }

    #[test]
    fn leader_keys() {
        assert_eq!(after_leader(b"-"), Some(Cmd::SplitDown));
        assert_eq!(after_leader(b"2"), Some(Cmd::GoTab(1)));
        assert_eq!(after_leader(b"\x1b[C"), Some(Cmd::Focus(Dir::Right)));
        assert_eq!(after_leader(b"k"), None);
    }

    #[test]
    fn option_chars() {
        assert!(is_option_char("¡".as_bytes()));
        assert!(is_option_char("†".as_bytes()));
        assert!(!is_option_char(b"a"));
    }
}
