use serde::Serialize;

macro_rules! text_enum {
    ($name:ident, $fallback:ident, { $($variant:ident => $text:literal),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name {
            $($variant),+
        }

        impl $name {
            pub fn as_str(self) -> &'static str {
                match self {
                    $($name::$variant => $text),+
                }
            }

            pub fn parse(text: &str) -> Self {
                match text {
                    $($text => $name::$variant,)+
                    _ => $name::$fallback,
                }
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

text_enum!(Trigger, Push, {
    Push => "push",
    Tag => "tag",
    PullRequest => "pull_request",
    Manual => "manual",
});

text_enum!(BuildState, Errored, {
    Queued => "queued",
    Starting => "starting",
    Running => "running",
    Releasing => "releasing",
    Succeeded => "succeeded",
    Failed => "failed",
    Cancelled => "cancelled",
    Superseded => "superseded",
    Errored => "errored",
    Skipped => "skipped",
});

impl BuildState {
    pub fn is_finished(self) -> bool {
        matches!(
            self,
            BuildState::Succeeded
                | BuildState::Failed
                | BuildState::Cancelled
                | BuildState::Superseded
                | BuildState::Errored
                | BuildState::Skipped
        )
    }

    pub fn is_active(self) -> bool {
        matches!(self, BuildState::Starting | BuildState::Running | BuildState::Releasing)
    }
}

text_enum!(Phase, Fetch, {
    Fetch => "fetch",
    Test => "test",
    Build => "build",
    Verify => "verify",
    Release => "release",
});

text_enum!(Failure, Infra, {
    Config => "config",
    Step => "step",
    Timeout => "timeout",
    OutOfMemory => "oom",
    Storage => "storage",
    Image => "image",
    Version => "version",
    Release => "release",
    Infra => "infra",
});

text_enum!(TaskKind, Test, {
    Fetch => "fetch",
    Test => "test",
    Image => "image",
    Push => "push",
    Verify => "verify",
});

text_enum!(TaskState, Pending, {
    Pending => "pending",
    Running => "running",
    Passed => "passed",
    Failed => "failed",
    Skipped => "skipped",
    Cancelled => "cancelled",
    TimedOut => "timed_out",
});

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EtagKind {
    Head,
    Tags,
    Pulls,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoSync {
    pub id: i64,
    pub installation_id: i64,
    pub owner: String,
    pub name: String,
    pub default_branch: String,
    pub private: bool,
    pub archived: bool,
    pub seen_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Repo {
    pub id: i64,
    pub installation_id: i64,
    pub owner: String,
    pub name: String,
    pub default_branch: String,
    pub private: bool,
    pub archived: bool,
    pub enrolled: bool,
    pub paused: bool,
    pub etag_head: Option<String>,
    pub etag_tags: Option<String>,
    pub etag_pulls: Option<String>,
    pub head_sha: Option<String>,
    pub rc_high_water: i64,
    pub config_error: Option<String>,
    pub last_seen_at: Option<i64>,
    pub removed_at: Option<i64>,
}

impl Repo {
    pub fn full_name(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewBuild {
    pub repo_id: i64,
    pub trigger: Trigger,
    pub ref_name: String,
    pub pr_number: Option<i64>,
    pub sha: String,
    pub message: Option<String>,
    pub dedupe_key: String,
    pub actor: Option<String>,
    pub trusted: bool,
    pub config: Option<String>,
    pub state: BuildState,
    pub failure: Option<Failure>,
    pub detail: Option<String>,
    pub rerun_of: Option<i64>,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Build {
    pub id: i64,
    pub repo_id: i64,
    pub number: i64,
    pub trigger: Trigger,
    pub ref_name: String,
    pub pr_number: Option<i64>,
    pub sha: String,
    pub message: Option<String>,
    pub actor: Option<String>,
    pub trusted: bool,
    pub config: Option<String>,
    pub state: BuildState,
    pub phase: Option<Phase>,
    pub failure: Option<Failure>,
    pub detail: Option<String>,
    pub attempt: i64,
    pub not_before: i64,
    pub pod: Option<String>,
    pub version: Option<String>,
    pub rerun_of: Option<i64>,
    pub cancel_requested: bool,
    pub created_at: i64,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
}

impl Build {
    pub fn short_sha(&self) -> &str {
        &self.sha[..self.sha.len().min(7)]
    }

    pub fn title(&self) -> &str {
        self.message.as_deref().and_then(|m| m.lines().next()).unwrap_or("")
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BuildFilter {
    pub repo: Option<i64>,
    pub before: Option<i64>,
    pub states: Vec<BuildState>,
    pub sha: Option<String>,
    pub limit: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BuildUpdate {
    pub state: Option<BuildState>,
    pub phase: Option<Option<Phase>>,
    pub failure: Option<Option<Failure>>,
    pub detail: Option<Option<String>>,
    pub pod: Option<Option<String>>,
    pub version: Option<Option<String>>,
    pub config: Option<Option<String>>,
    pub attempt: Option<i64>,
    pub not_before: Option<i64>,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    pub cancel_requested: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskPlan {
    pub kind: TaskKind,
    pub name: String,
    pub container: String,
    pub steps: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Task {
    pub ordinal: i64,
    pub kind: TaskKind,
    pub name: String,
    pub container: String,
    pub state: TaskState,
    pub exit_code: Option<i64>,
    pub reason: Option<String>,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    pub log_final: bool,
    pub log_bytes: Option<i64>,
    pub steps: Vec<Step>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Step {
    pub ordinal: i64,
    pub name: String,
    pub state: TaskState,
    pub exit_code: Option<i64>,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    pub log_offset: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskUpdate {
    pub state: Option<TaskState>,
    pub exit_code: Option<i64>,
    pub reason: Option<String>,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    pub log_final: Option<bool>,
    pub log_bytes: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepUpdate {
    pub state: TaskState,
    pub exit_code: Option<i64>,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    pub log_offset: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImageRecord {
    pub ordinal: i64,
    pub name: String,
    pub digest: Option<String>,
    pub released: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxItem {
    pub id: i64,
    pub build_id: i64,
    pub kind: String,
    pub payload: serde_json::Value,
    pub attempts: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub id_hash: String,
    pub user_id: i64,
    pub login: String,
    pub avatar_url: String,
    pub created_at: i64,
    pub seen_at: i64,
    pub ip: Option<String>,
    pub agent: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AuditEntry {
    pub at: i64,
    pub actor: String,
    pub action: String,
    pub repo_id: Option<i64>,
    pub build_id: Option<i64>,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Retention {
    pub build_days: i64,
    pub builds_per_repo: i64,
    pub session_idle_secs: i64,
    pub session_max_secs: i64,
}

impl Default for Retention {
    fn default() -> Self {
        Self { build_days: 90, builds_per_repo: 200, session_idle_secs: 12 * 3600, session_max_secs: 7 * 86_400 }
    }
}
