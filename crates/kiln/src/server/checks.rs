use kiln_core::config::FILE_NAME;
use kiln_github::{Annotation, CheckConclusion, CheckOutput, CheckRun, CheckStatus};
use kiln_store::{Build, BuildState, Failure, ImageRecord, Phase, Repo, Task, TaskKind, TaskState, Trigger};

pub const CHECK_NAME: &str = "kiln";

pub fn build_url(public_url: &str, repo: &Repo, build: &Build) -> String {
    format!("{public_url}/{}/{}/builds/{}", repo.owner, repo.name, build.number)
}

pub fn phase_label(phase: Option<Phase>) -> &'static str {
    match phase {
        Some(Phase::Fetch) | None => "Fetching the source",
        Some(Phase::Test) => "Running tests",
        Some(Phase::Build) => "Building images",
        Some(Phase::Verify) => "Verifying images",
        Some(Phase::Release) => "Releasing",
    }
}

pub fn headline(build: &Build) -> String {
    let detail = build.detail.clone().unwrap_or_default();
    match build.state {
        BuildState::Queued if detail.starts_with("Retrying") => {
            format!("Retrying: {}", detail.trim_start_matches("Retrying: "))
        }
        BuildState::Queued => "Waiting for a build slot".into(),
        BuildState::Starting | BuildState::Running => phase_label(build.phase).into(),
        BuildState::Releasing => format!("Releasing {}", build.version.clone().unwrap_or_default()),
        BuildState::Succeeded if !detail.is_empty() => detail,
        BuildState::Succeeded if build.trigger == Trigger::PullRequest => "Tests passed".into(),
        BuildState::Succeeded => "Passed".into(),
        BuildState::Failed => match build.failure {
            Some(Failure::Config) => format!("{FILE_NAME} has errors"),
            _ => first_line(&detail).unwrap_or("Failed").to_owned(),
        },
        BuildState::Errored => {
            format!("Could not run: {}", first_line(&detail).unwrap_or("unknown error"))
        }
        BuildState::Cancelled | BuildState::Superseded | BuildState::Skipped => {
            first_line(&detail).unwrap_or(build.state.as_str()).to_owned()
        }
    }
}

fn first_line(text: &str) -> Option<&str> {
    text.lines().next().filter(|l| !l.is_empty())
}

pub fn task_title(task: &Task) -> String {
    match task.kind {
        TaskKind::Fetch => "Fetch source".into(),
        TaskKind::Test => format!("Test · {}", task.name),
        TaskKind::Image => format!("Build image · {}", task.name),
        TaskKind::Push => "Push candidates".into(),
        TaskKind::Verify => format!("Verify · {}", task.name),
    }
}

fn icon(state: TaskState) -> &'static str {
    match state {
        TaskState::Passed => "✅",
        TaskState::Failed | TaskState::TimedOut => "❌",
        TaskState::Running => "⏳",
        TaskState::Cancelled => "⛔",
        TaskState::Pending | TaskState::Skipped => "⚪",
    }
}

pub fn duration(seconds: i64) -> String {
    match seconds {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 && s % 60 == 0 => format!("{}m", s / 60),
        s if s < 3600 => format!("{}m {}s", s / 60, s % 60),
        s => format!("{}h {}m", s / 3600, (s % 3600) / 60),
    }
}

pub fn check_run(build: &Build, repo: &Repo, tasks: &[Task], images: &[ImageRecord], public_url: &str) -> CheckRun {
    let url = build_url(public_url, repo, build);
    let (status, conclusion) = match build.state {
        BuildState::Queued => (CheckStatus::Queued, None),
        BuildState::Starting | BuildState::Running | BuildState::Releasing => (CheckStatus::InProgress, None),
        BuildState::Succeeded => (CheckStatus::Completed, Some(CheckConclusion::Success)),
        BuildState::Failed | BuildState::Errored => (CheckStatus::Completed, Some(CheckConclusion::Failure)),
        BuildState::Cancelled => (CheckStatus::Completed, Some(CheckConclusion::Cancelled)),
        BuildState::Superseded | BuildState::Skipped => (CheckStatus::Completed, Some(CheckConclusion::Skipped)),
    };
    let mut summary = format!("**Build #{}** of `{}` · [open in Kiln]({url})\n\n", build.number, build.short_sha());
    if let Some(detail) = &build.detail
        && build.failure != Some(Failure::Config)
    {
        summary.push_str(&format!("{detail}\n\n"));
    }
    if !tasks.is_empty() {
        summary.push_str("| | Task | Time |\n|---|---|---|\n");
        for task in tasks {
            let time = match (task.started_at, task.finished_at) {
                (Some(start), Some(end)) => duration(end - start),
                (Some(_), None) => "running".into(),
                _ => String::new(),
            };
            summary.push_str(&format!("| {} | {} | {time} |\n", icon(task.state), task_title(task)));
            for step in task.steps.iter().filter(|s| matches!(s.state, TaskState::Failed | TaskState::TimedOut)) {
                let code = step.exit_code.map(|c| format!(" (exit {c})")).unwrap_or_default();
                summary.push_str(&format!("| | ↳ `{}`{code} | |\n", step.name.replace('|', "\\|")));
            }
        }
    }
    let released: Vec<String> =
        images.iter().filter_map(|i| i.released.as_ref().map(|t| format!("`{}:{t}`", i.name))).collect();
    if !released.is_empty() {
        summary.push_str(&format!("\nReleased {}\n", released.join(", ")));
    }
    let annotations = if build.failure == Some(Failure::Config) {
        build.detail.as_deref().map(config_annotations).unwrap_or_default()
    } else {
        Vec::new()
    };
    if build.failure == Some(Failure::Config)
        && let Some(detail) = &build.detail
    {
        summary.push_str(&format!("\n```\n{detail}\n```\n"));
    }
    CheckRun {
        name: CHECK_NAME.into(),
        head_sha: build.sha.clone(),
        status,
        conclusion,
        details_url: Some(url),
        external_id: Some(build.id.to_string()),
        output: Some(CheckOutput { title: headline(build), summary, text: None, annotations }),
    }
}

fn config_annotations(detail: &str) -> Vec<Annotation> {
    detail
        .lines()
        .filter_map(|line| {
            let rest = line.strip_prefix(&format!("{FILE_NAME}:"))?;
            let (number, message) = rest.split_once(": ")?;
            let line_number: u32 = number.parse().ok()?;
            Some(Annotation {
                path: FILE_NAME.into(),
                start_line: line_number,
                end_line: line_number,
                annotation_level: "failure",
                message: message.to_owned(),
                title: "Invalid Kiln configuration".into(),
            })
        })
        .collect()
}
