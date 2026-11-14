#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Marker {
    StepStart { index: usize, at: i64 },
    StepEnd { index: usize, exit: i32, at: i64 },
    Image { name: String, digest: String },
}

const PREFIX: &str = "##kiln[";

pub fn render_marker(marker: &Marker) -> String {
    match marker {
        Marker::StepStart { index, at } => format!("{PREFIX}step-start index={index} at={at}]"),
        Marker::StepEnd { index, exit, at } => format!("{PREFIX}step-end index={index} exit={exit} at={at}]"),
        Marker::Image { name, digest } => format!("{PREFIX}image name={name} digest={digest}]"),
    }
}

pub fn parse_marker(line: &str) -> Option<Marker> {
    let body = line.trim_end().strip_prefix(PREFIX)?.strip_suffix(']')?;
    let (kind, rest) = body.split_once(' ')?;
    let field = |key: &str| {
        rest.split(' ').find_map(|pair| pair.split_once('=').filter(|(k, _)| *k == key).map(|(_, v)| v.to_owned()))
    };
    match kind {
        "step-start" => {
            Some(Marker::StepStart { index: field("index")?.parse().ok()?, at: field("at")?.parse().ok()? })
        }
        "step-end" => Some(Marker::StepEnd {
            index: field("index")?.parse().ok()?,
            exit: field("exit")?.parse().ok()?,
            at: field("at")?.parse().ok()?,
        }),
        "image" => {
            let digest = field("digest")?;
            let name = field("name")?;
            digest.starts_with("sha256:").then_some(Marker::Image { name, digest })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markers_round_trip_and_ignore_lookalikes() {
        let markers = [
            Marker::StepStart { index: 2, at: 100 },
            Marker::StepEnd { index: 2, exit: 137, at: 160 },
            Marker::Image { name: "web".into(), digest: format!("sha256:{}", "a".repeat(64)) },
        ];
        for marker in markers {
            assert_eq!(parse_marker(&render_marker(&marker)), Some(marker));
        }
        assert_eq!(parse_marker("echo ##kiln[step-end index=0 exit=0 at=1]"), None);
        assert_eq!(parse_marker("##kiln[image name=web digest=md5:abc]"), None);
    }
}
