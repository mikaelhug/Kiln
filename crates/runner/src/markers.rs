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

pub fn cache_counts(log: &str) -> Option<(usize, usize)> {
    let mut steps = std::collections::BTreeSet::new();
    let mut cached = std::collections::BTreeSet::new();
    for line in log.lines() {
        let Some(rest) = line.strip_prefix('#') else { continue };
        let Some((id, body)) = rest.split_once(' ') else { continue };
        let Ok(id) = id.parse::<u32>() else { continue };
        if body.starts_with('[') && !body.starts_with("[internal]") {
            steps.insert(id);
        } else if body.trim_end() == "CACHED" {
            cached.insert(id);
        }
    }
    (!steps.is_empty()).then(|| (steps.intersection(&cached).count(), steps.len()))
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

    #[test]
    fn cache_counts_build_steps_that_buildkit_reused() {
        let log = "#1 [internal] load build definition from Dockerfile\n#1 DONE 0.0s\n\
#5 [deps 1/3] FROM docker.io/library/node:26-slim@sha256:abc\n#5 CACHED\n\
#6 [deps 2/3] COPY package.json ./\n#6 CACHED\n\
#7 [deps 3/3] RUN npm ci\n#7 12.3 added 812 packages\n#7 DONE 14.1s\n\
#8 exporting to oci image format\n#8 DONE 2.4s\n";
        assert_eq!(cache_counts(log), Some((2, 3)));
        assert_eq!(cache_counts("Fetching the source\n"), None);
    }
}
