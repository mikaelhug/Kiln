mod markers;
mod observe;
mod pod;

pub use k8s_openapi;
pub use markers::{Marker, parse_marker, render_marker};
pub use observe::{Observation, Outcome, observe};
pub use pod::{Plan, Rendered, Scope, Settings, StepSpec, cache_claim_name, candidate_ref, render};

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
