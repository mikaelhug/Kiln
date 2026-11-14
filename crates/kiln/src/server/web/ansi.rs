use maud::{Markup, html};

pub fn line(raw: &str) -> Markup {
    let visible = raw.rsplit('\r').find(|part| !part.is_empty()).unwrap_or("");
    let class = if visible.starts_with("$ ") { "line cmd" } else { "line" };
    html! { div class=(class) { (spans(visible)) } }
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
        let rendered = line("\u{1b}[1;32m✓ passed\u{1b}[0m <script>").into_string();
        assert_eq!(rendered, r#"<div class="line"><span class="a-b a-32">✓ passed</span> &lt;script&gt;</div>"#);
        let progress = line("downloading 10%\rdownloading 100%").into_string();
        assert!(progress.contains("downloading 100%") && !progress.contains("10%"));
    }
}
