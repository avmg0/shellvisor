//! A tiny syntax highlighter for the file preview: comments, strings,
//! numbers and keywords for common languages. Deliberately small — no
//! grammar bundles, no regex engine.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

const KEYWORD: Color = Color::Indexed(176);
const STRING: Color = Color::Indexed(108);
const COMMENT: Color = Color::Indexed(244);
const NUMBER: Color = Color::Indexed(179);
const HEADING: Color = Color::Indexed(73);

struct Lang {
    keywords: &'static [&'static str],
    comments: &'static [&'static str],
    quotes: &'static [char],
}

const RUST: Lang = Lang {
    keywords: &[
        "as", "async", "await", "break", "const", "continue", "crate", "else", "enum", "false",
        "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub",
        "ref", "return", "self", "Self", "static", "struct", "super", "trait", "true", "type",
        "unsafe", "use", "where", "while", "dyn",
    ],
    comments: &["//"],
    quotes: &['"'],
};
const PYTHON: Lang = Lang {
    keywords: &[
        "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del",
        "elif", "else", "except", "False", "finally", "for", "from", "global", "if", "import",
        "in", "is", "lambda", "None", "nonlocal", "not", "or", "pass", "raise", "return", "True",
        "try", "while", "with", "yield", "self",
    ],
    comments: &["#"],
    quotes: &['"', '\''],
};
const JS: Lang = Lang {
    keywords: &[
        "async",
        "await",
        "break",
        "case",
        "catch",
        "class",
        "const",
        "continue",
        "default",
        "delete",
        "do",
        "else",
        "export",
        "extends",
        "false",
        "finally",
        "for",
        "from",
        "function",
        "if",
        "import",
        "in",
        "instanceof",
        "interface",
        "let",
        "new",
        "null",
        "of",
        "return",
        "static",
        "super",
        "switch",
        "this",
        "throw",
        "true",
        "try",
        "type",
        "typeof",
        "undefined",
        "var",
        "void",
        "while",
        "yield",
    ],
    comments: &["//"],
    quotes: &['"', '\'', '`'],
};
const GO: Lang = Lang {
    keywords: &[
        "break",
        "case",
        "chan",
        "const",
        "continue",
        "default",
        "defer",
        "else",
        "false",
        "for",
        "func",
        "go",
        "goto",
        "if",
        "import",
        "interface",
        "map",
        "nil",
        "package",
        "range",
        "return",
        "select",
        "struct",
        "switch",
        "true",
        "type",
        "var",
    ],
    comments: &["//"],
    quotes: &['"', '`'],
};
const C: Lang = Lang {
    keywords: &[
        "auto",
        "break",
        "case",
        "char",
        "class",
        "const",
        "continue",
        "default",
        "do",
        "double",
        "else",
        "enum",
        "extern",
        "false",
        "float",
        "for",
        "if",
        "int",
        "long",
        "namespace",
        "new",
        "nullptr",
        "private",
        "public",
        "return",
        "short",
        "signed",
        "sizeof",
        "static",
        "struct",
        "switch",
        "template",
        "true",
        "typedef",
        "union",
        "unsigned",
        "void",
        "while",
        "include",
        "define",
    ],
    comments: &["//"],
    quotes: &['"', '\''],
};
const SHELL: Lang = Lang {
    keywords: &[
        "case", "do", "done", "elif", "else", "esac", "export", "fi", "for", "function", "if",
        "in", "local", "return", "then", "until", "while", "echo", "source",
    ],
    comments: &["#"],
    quotes: &['"', '\''],
};
const DATA: Lang = Lang {
    keywords: &["true", "false", "null", "yes", "no"],
    comments: &["#"],
    quotes: &['"', '\''],
};
const JSON: Lang = Lang {
    keywords: &["true", "false", "null"],
    comments: &[],
    quotes: &['"'],
};

fn lang_for(ext: &str) -> Option<&'static Lang> {
    Some(match ext {
        "rs" => &RUST,
        "py" | "pyi" => &PYTHON,
        "js" | "jsx" | "ts" | "tsx" | "mjs" | "cjs" | "java" | "kt" | "swift" | "cs" => &JS,
        "go" => &GO,
        "c" | "h" | "cc" | "cpp" | "hpp" | "m" => &C,
        "sh" | "bash" | "zsh" | "fish" => &SHELL,
        "yaml" | "yml" | "toml" | "ini" | "conf" | "cfg" | "env" => &DATA,
        "json" => &JSON,
        _ => return None,
    })
}

/// Highlight `text` according to the file extension `ext`.
pub fn highlight(text: &str, ext: &str) -> Vec<Line<'static>> {
    let ext = ext.to_ascii_lowercase();
    if ext == "md" || ext == "markdown" {
        return text.lines().map(markdown_line).collect();
    }
    match lang_for(&ext) {
        Some(l) => text
            .lines()
            .map(|line| code_line(&expand(line), l))
            .collect(),
        None => text.lines().map(|l| Line::raw(expand(l))).collect(),
    }
}

fn expand(line: &str) -> String {
    line.replace('\t', "    ")
}

fn markdown_line(line: &str) -> Line<'static> {
    let l = expand(line);
    if l.starts_with('#') {
        Line::styled(l, Style::default().fg(HEADING).add_modifier(Modifier::BOLD))
    } else if l.trim_start().starts_with("```") {
        Line::styled(l, Style::default().fg(COMMENT))
    } else {
        Line::raw(l)
    }
}

fn code_line(line: &str, lang: &Lang) -> Line<'static> {
    let chars: Vec<char> = line.chars().collect();
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut plain = String::new();
    let flush = |plain: &mut String, spans: &mut Vec<Span<'static>>| {
        if !plain.is_empty() {
            spans.push(Span::raw(std::mem::take(plain)));
        }
    };
    let mut i = 0;
    while i < chars.len() {
        let rest: String = chars[i..].iter().collect();
        if lang.comments.iter().any(|c| rest.starts_with(c)) {
            flush(&mut plain, &mut spans);
            spans.push(Span::styled(
                rest,
                Style::default().fg(COMMENT).add_modifier(Modifier::ITALIC),
            ));
            break;
        }
        let c = chars[i];
        if lang.quotes.contains(&c) {
            flush(&mut plain, &mut spans);
            let mut j = i + 1;
            while j < chars.len() && chars[j] != c {
                if chars[j] == '\\' {
                    j += 1;
                }
                j += 1;
            }
            let end = (j + 1).min(chars.len());
            spans.push(Span::styled(
                chars[i..end].iter().collect::<String>(),
                Style::default().fg(STRING),
            ));
            i = end;
            continue;
        }
        if c.is_ascii_digit() && (i == 0 || !is_word(chars[i - 1])) {
            flush(&mut plain, &mut spans);
            let mut j = i;
            while j < chars.len()
                && (chars[j].is_ascii_alphanumeric() || chars[j] == '.' || chars[j] == '_')
            {
                j += 1;
            }
            spans.push(Span::styled(
                chars[i..j].iter().collect::<String>(),
                Style::default().fg(NUMBER),
            ));
            i = j;
            continue;
        }
        if is_word(c) && (i == 0 || !is_word(chars[i - 1])) {
            let mut j = i;
            while j < chars.len() && is_word(chars[j]) {
                j += 1;
            }
            let word: String = chars[i..j].iter().collect();
            if lang.keywords.contains(&word.as_str()) {
                flush(&mut plain, &mut spans);
                spans.push(Span::styled(word, Style::default().fg(KEYWORD)));
            } else {
                plain.push_str(&word);
            }
            i = j;
            continue;
        }
        plain.push(c);
        i += 1;
    }
    flush(&mut plain, &mut spans);
    Line::from(spans)
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parts(l: &Line) -> Vec<(String, Option<Color>)> {
        l.spans
            .iter()
            .map(|s| (s.content.to_string(), s.style.fg))
            .collect()
    }

    #[test]
    fn rust_line() {
        let l = &highlight("let x = \"hi\"; // note 42", "rs")[0];
        let p = parts(l);
        assert_eq!(p[0], ("let".into(), Some(KEYWORD)));
        assert!(p.contains(&("\"hi\"".into(), Some(STRING))));
        assert_eq!(p.last().unwrap(), &("// note 42".into(), Some(COMMENT)));
    }

    #[test]
    fn python_numbers_and_strings() {
        let p = parts(&highlight("def f(a1): return 3.5 + 'x\\'y'", "py")[0]);
        assert_eq!(p[0], ("def".into(), Some(KEYWORD)));
        assert!(p.contains(&("3.5".into(), Some(NUMBER))));
        assert!(p.contains(&("'x\\'y'".into(), Some(STRING))));
        // "a1" is an identifier, not a number
        assert!(!p.iter().any(|(t, c)| t == "1" && *c == Some(NUMBER)));
    }

    #[test]
    fn unknown_and_markdown() {
        assert_eq!(highlight("a\tb", "xyz")[0].spans[0].content, "a    b");
        let md = highlight("# Title\nbody", "md");
        assert_eq!(md[0].style.fg, Some(HEADING));
        assert_eq!(md.len(), 2);
    }

    #[test]
    fn unterminated_string_does_not_panic() {
        let p = parts(&highlight("x = \"open", "py")[0]);
        assert_eq!(p.last().unwrap().0, "\"open");
    }
}
