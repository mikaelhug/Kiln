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
    html! { div class=(class) id=(anchor) { a class="ln" href={"#" (anchor)} { (number) } span class="tx" { (spans(visible)) } } }
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

fn spans(text: &str) -> Markup {
    let mut segments: Vec<(Vec<&'static str>, String)> = Vec::new();
    let mut classes: Vec<&'static str> = Vec::new();
    let mut current = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            current.push(c);
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
                code.push(next);
            } else {
                terminator = Some(next);
                break;
            }
        }
        if terminator != Some('m') {
            continue;
        }
        if !current.is_empty() {
            segments.push((classes.clone(), std::mem::take(&mut current)));
        }
        for part in code.split(';') {
            match part.parse::<u8>().unwrap_or(0) {
                0 => classes.clear(),
                1 => classes.push("a-b"),
                2 => classes.push("a-d"),
                n @ 30..=37 => {
                    classes.retain(|c| !c.starts_with("a-3"));
                    classes.push(COLORS[usize::from(n - 30)]);
                }
                n @ 90..=97 => {
                    classes.retain(|c| !c.starts_with("a-3"));
                    classes.push(COLORS[usize::from(n - 90)]);
                }
                39 => classes.retain(|c| !c.starts_with("a-3")),
                22 => classes.retain(|c| *c != "a-b" && *c != "a-d"),
                _ => {}
            }
        }
    }
    if !current.is_empty() {
        segments.push((classes, current));
    }
    html! {
        @for (classes, text) in &segments {
            @if classes.is_empty() { (text) } @else { span class=(classes.join(" ")) { (text) } }
        }
    }
}

const COLORS: [&str; 8] = ["a-30", "a-31", "a-32", "a-33", "a-34", "a-35", "a-36", "a-37"];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_are_rendered_and_markup_is_escaped() {
        let rendered = line(2, 7, "\u{1b}[1;32m✓ passed\u{1b}[0m <script>").into_string();
        assert!(rendered.contains(r#"<span class="tx"><span class="a-b a-32">✓ passed</span> &lt;script&gt;</span>"#));
        let progress = line(2, 8, "downloading 10%\rdownloading 100%").into_string();
        assert!(progress.contains("downloading 100%") && !progress.contains("10%"));
    }
}
