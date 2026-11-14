use maud::{Markup, html};

pub fn line(task: i64, number: usize, raw: &str) -> Markup {
    let visible = raw.rsplit('\r').find(|part| !part.is_empty()).unwrap_or("");
    let class = if visible.starts_with("$ ") {
        "line cmd"
    } else if is_error(&strip(visible)) {
        "line err"
    } else {
        "line"
    };
    let anchor = format!("task-{task}-L{number}");
    html! { div class=(class) id=(anchor) { a class="ln" href={"#" (anchor)} tabindex="-1" aria-hidden="true" { (number) } span class="tx" { (spans(visible)) } } }
}

fn strip(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            for next in chars.by_ref() {
                if next.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn is_error(text: &str) -> bool {
    let lower = text.trim_start().to_lowercase();
    ["error", "fatal", "panic", "✗", "×", "✕"].iter().any(|p| lower.starts_with(p))
        || lower.contains("error:")
        || lower.contains("error[")
        || text.split(|c: char| !c.is_ascii_alphabetic()).any(|w| w == "FAIL" || w == "FAILED")
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct Style {
    bold: bool,
    dim: bool,
    color: Option<u8>,
}

impl Style {
    fn class(self) -> String {
        let mut parts = Vec::new();
        if self.bold {
            parts.push("a-b".to_owned());
        }
        if self.dim {
            parts.push("a-d".to_owned());
        }
        if let Some(color) = self.color {
            parts.push(format!("a-3{color}"));
        }
        parts.join(" ")
    }

    fn apply(&mut self, code: &str) {
        let parts: Vec<&str> = code.split(';').collect();
        let mut index = 0;
        while index < parts.len() {
            let part = parts[index];
            index += 1;
            match part.parse::<u8>().unwrap_or(0) {
                38 | 48 => {
                    index += match parts.get(index) {
                        Some(&"5") => 2,
                        Some(&"2") => 4,
                        _ => 0,
                    };
                }
                0 => *self = Style::default(),
                1 => self.bold = true,
                2 => self.dim = true,
                22 => (self.bold, self.dim) = (false, false),
                n @ (30..=37 | 90..=97) => self.color = Some(n % 10),
                39 => self.color = None,
                _ => {}
            }
        }
    }
}

fn spans(text: &str) -> Markup {
    let mut segments: Vec<(Style, String)> = Vec::new();
    let mut style = Style::default();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            match segments.last_mut() {
                Some((last, current)) if *last == style => current.push(c),
                _ => segments.push((style, c.to_string())),
            }
            continue;
        }
        if chars.peek() != Some(&'[') {
            continue;
        }
        chars.next();
        let mut code = String::new();
        let mut terminator = None;
        for next in chars.by_ref() {
            if next.is_ascii_digit() || next == ';' {
                if code.len() < 64 {
                    code.push(next);
                }
            } else {
                terminator = Some(next);
                break;
            }
        }
        if terminator == Some('m') {
            style.apply(&code);
        }
    }
    html! {
        @for (style, text) in &segments {
            @if *style == Style::default() { (text) } @else { span class=(style.class()) { (text) } }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_are_rendered_and_markup_is_escaped() {
        let rendered = line(2, 7, "\u{1b}[1;32m✓ passed\u{1b}[0m <script>").into_string();
        assert!(rendered.contains(r#"<span class="tx"><span class="a-b a-32">✓ passed</span> &lt;script&gt;</span>"#));
        let progress = line(2, 8, "downloading 10%\rdownloading 100%").into_string();
        assert!(progress.contains("downloading 100%") && !progress.contains("10%"));
        let truecolor = line(2, 9, "\u{1b}[38;2;255;32;0mwarm\u{1b}[0m").into_string();
        assert!(truecolor.contains(r#"<span class="tx">warm</span>"#), "{truecolor}");
        let flood = line(2, 10, &"\u{1b}[1mX".repeat(10_000)).into_string();
        assert!(flood.len() < 20_000, "repeated styles stay one span: {} bytes", flood.len());
    }
}
