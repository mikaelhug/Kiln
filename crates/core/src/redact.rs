use base64::Engine;
use base64::engine::general_purpose::{STANDARD_NO_PAD, URL_SAFE_NO_PAD};
use regex::Regex;

pub const MASK: &str = "***";
const MIN_LEN: usize = 6;
const TOKEN_PATTERNS: &str = r"gh[pousr]_[A-Za-z0-9]{36,}|github_pat_[A-Za-z0-9_]{22,}";

pub struct Redactor {
    pattern: Regex,
}

impl Redactor {
    pub fn new<S: AsRef<str>>(secrets: &[S]) -> Self {
        let mut needles: Vec<String> = secrets.iter().flat_map(|secret| variants(secret.as_ref())).collect();
        needles.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
        needles.dedup();
        let mut alternatives: Vec<String> = needles.iter().map(|needle| regex::escape(needle)).collect();
        alternatives.push(TOKEN_PATTERNS.to_owned());
        let pattern = regex::RegexBuilder::new(&alternatives.join("|"))
            .size_limit(1 << 28)
            .dfa_size_limit(1 << 26)
            .build()
            .expect("escaped alternation is a valid regex");
        Self { pattern }
    }

    pub fn redact(&self, line: &str) -> String {
        self.pattern.replace_all(line, MASK).into_owned()
    }
}

fn variants(secret: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut push = |value: String| {
        if value.len() >= MIN_LEN {
            out.push(value);
        }
    };
    push(secret.to_owned());
    for line in secret.lines() {
        push(line.trim().to_owned());
    }
    if secret.len() < MIN_LEN {
        return out;
    }
    let bytes = secret.as_bytes();
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    push(hex.to_ascii_uppercase());
    push(hex);
    push(percent_encode(secret));
    if let Ok(json) = serde_json::to_string(secret) {
        push(json.trim_matches('"').to_owned());
    }
    for engine in [&STANDARD_NO_PAD, &URL_SAFE_NO_PAD] {
        for offset in 0..3usize {
            let mut shifted = vec![0u8; offset];
            shifted.extend_from_slice(bytes);
            let encoded = engine.encode(&shifted);
            let start = (offset * 4).div_ceil(3);
            let end = ((offset + bytes.len()) * 4 / 3).min(encoded.len());
            if end > start {
                push(encoded[start..end].to_owned());
            }
        }
    }
    out
}

fn percent_encode(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => (byte as char).to_string(),
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use base64::engine::general_purpose::STANDARD;

    use super::*;

    const SECRET: &str = "s3cr3t/value+with=chars";

    #[test]
    fn secrets_are_masked_in_every_common_encoding() {
        let redactor = Redactor::new(&[SECRET, "-----BEGIN KEY-----\nAAAAbbbbCCCC\n-----END KEY-----"]);
        let encodings = [
            SECRET.to_owned(),
            STANDARD.encode(SECRET),
            STANDARD.encode(format!("x{SECRET}")),
            STANDARD.encode(format!("xy{SECRET}")),
            URL_SAFE_NO_PAD.encode(format!("token={SECRET}")),
            SECRET.bytes().map(|b| format!("{b:02x}")).collect(),
            percent_encode(SECRET),
            "AAAAbbbbCCCC".to_owned(),
        ];
        for encoded in encodings {
            let line = format!("value: {encoded} end");
            let redacted = redactor.redact(&line);
            assert!(redacted.contains(MASK), "{encoded} survived as {redacted}");
            assert!(!redacted.contains(&encoded), "{encoded} survived as {redacted}");
        }
    }

    #[test]
    fn github_tokens_are_masked_without_being_configured() {
        let redactor = Redactor::new::<&str>(&[]);
        let token = format!("ghs_{}", "a1".repeat(18));
        assert_eq!(redactor.redact(&format!("Authorization: token {token}")), "Authorization: token ***");
        assert_eq!(redactor.redact("npm ci finished"), "npm ci finished");
    }
}
