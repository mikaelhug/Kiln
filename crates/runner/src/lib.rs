mod markers;
mod observe;
mod pod;

pub use k8s_openapi;
pub use markers::{Marker, parse_marker, render_marker};
pub use observe::{Observation, Outcome, observe};
pub use pod::{Plan, Rendered, Scope, Settings, StepSpec, cache_claim_name, candidate_ref, render};

use sha2::{Digest, Sha256};

pub const MANAGED_BY: &str = "app.kubernetes.io/managed-by";
pub const BUILD_LABEL: &str = "kiln.dev/build";
pub const ATTEMPT_LABEL: &str = "kiln.dev/attempt";
pub const REPO_LABEL: &str = "kiln.dev/repo";

pub fn pod_name(build: i64, attempt: i64) -> String {
    format!("kiln-b{build}-a{attempt}")
}

pub fn label_value(text: &str) -> String {
    let mut out: String =
        text.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '.' || c == '_' { c.to_ascii_lowercase() } else { '-' }
            })
            .collect();
    out.truncate(63);
    out.trim_matches(|c: char| !c.is_ascii_alphanumeric()).to_owned()
}

pub fn resource_name(prefix: &str, value: &str) -> String {
    let lower = value.to_ascii_lowercase();
    let hash = hex::encode(&Sha256::digest(lower.as_bytes())[..4]);
    let mut slug = String::new();
    for c in lower.chars() {
        let c = if c.is_ascii_alphanumeric() { c } else { '-' };
        if c != '-' || !slug.ends_with('-') {
            slug.push(c);
        }
    }
    let room = 63usize.saturating_sub(prefix.len() + hash.len() + 1);
    slug.truncate(room);
    let slug = slug.trim_matches('-');
    if slug.is_empty() { format!("{prefix}{hash}") } else { format!("{prefix}{slug}-{hash}") }
}

#[cfg(test)]
mod tests {
    use super::resource_name;

    #[test]
    fn resource_names_are_valid_and_unique_per_repository() {
        let names: Vec<String> =
            ["acme/my_service", "acme/a-b", "acme-a/b", "Acme/A-B", &format!("bigco/{}", "x".repeat(80))]
                .iter()
                .map(|repo| resource_name("kiln-cache-", repo))
                .collect();
        for name in &names {
            assert!(name.len() <= 63, "{name}");
            assert!(name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'), "{name}");
            assert!(name.ends_with(|c: char| c.is_ascii_alphanumeric()), "{name}");
        }
        assert_ne!(names[1], names[2], "repositories that flatten alike keep distinct names");
        assert_eq!(names[1], names[3], "GitHub names are case-insensitive");
    }
}
