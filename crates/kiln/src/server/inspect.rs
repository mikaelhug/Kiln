use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use kiln_github::unix_now;
use kiln_runner::{Marker, parse_marker};
use kiln_store::{Build, BuildFilter, BuildState, Store, Task, TaskState};
use tokio::sync::broadcast;

use super::logs::Logs;
use super::settings::Settings;

fn open() -> Result<(Store, Logs)> {
    let dir = Settings::data_dir();
    let path = dir.join("kiln.db");
    if !path.exists() {
        bail!("no Kiln database at {}; run this inside the Kiln server pod", path.display());
    }
    let (events, _) = broadcast::channel(1);
    Ok((Store::open(&path)?, Logs::new(dir, events)))
}

fn split_repo(full: &str) -> Result<(&str, &str)> {
    full.split_once('/').with_context(|| format!("{full:?} is not owner/name"))
}

fn ago(at: i64) -> String {
    let secs = (unix_now() as i64 - at).max(0);
    match secs {
        0..60 => format!("{secs}s ago"),
        60..3600 => format!("{}m ago", secs / 60),
        3600..86_400 => format!("{}h ago", secs / 3600),
        _ => format!("{}d ago", secs / 86_400),
    }
}

fn summary(build: &Build) -> String {
    let mut line = format!(
        "#{} {} {} {} {}",
        build.number,
        build.state.as_str(),
        build.ref_name,
        build.short_sha(),
        build.version.as_deref().unwrap_or("-")
    );
    line.push_str(&format!(" {}", ago(build.created_at)));
    if let Some(detail) = build.detail.as_deref().and_then(|d| d.lines().next()) {
        line.push_str(&format!(" · {detail}"));
    }
    line
}

pub fn builds(repo: Option<String>, limit: i64) -> Result<ExitCode> {
    let (store, _) = open()?;
    let repos = store.repos()?;
    let only = match &repo {
        Some(full) => {
            let (owner, name) = split_repo(full)?;
            Some(store.repo_by_name(owner, name)?.with_context(|| format!("Kiln does not know {full}"))?.id)
        }
        None => None,
    };
    let filter = BuildFilter { repo: only, limit: limit.clamp(1, 200), ..BuildFilter::default() };
    for build in store.builds(&filter)? {
        let name = repos.iter().find(|r| r.id == build.repo_id).map(|r| r.full_name()).unwrap_or_default();
        if only.is_some() {
            println!("{}", summary(&build));
        } else {
            println!("{name} {}", summary(&build));
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn render(task: &Task, text: &str) -> String {
    let mut out = String::new();
    for line in text.lines() {
        match parse_marker(line) {
            Some(Marker::StepStart { index, .. }) => {
                let name = task.steps.get(index).map(|s| s.name.as_str()).unwrap_or("step");
                out.push_str(&format!("--- {name}\n"));
            }
            Some(Marker::StepEnd { index, exit, .. }) if exit != 0 => {
                let name = task.steps.get(index).map(|s| s.name.as_str()).unwrap_or("step");
                out.push_str(&format!("--- {name} exited with code {exit}\n"));
            }
            Some(_) => {}
            None => {
                out.push_str(line);
                out.push('\n');
            }
        }
    }
    out
}

fn header(task: &Task) -> String {
    let code = task.exit_code.filter(|c| *c != 0).map(|c| format!(", exit {c}")).unwrap_or_default();
    format!("=== {} ({}{code})", task.name, task.state.as_str())
}

pub async fn logs(repo: String, number: i64, task: Option<String>, follow: bool) -> Result<ExitCode> {
    let (store, logs) = open()?;
    let (owner, name) = split_repo(&repo)?;
    let found = store.repo_by_name(owner, name)?.with_context(|| format!("Kiln does not know {repo}"))?;
    let mut build =
        store.build_by_number(found.id, number)?.with_context(|| format!("{repo} has no build #{number}"))?;
    println!("{repo} {}", summary(&build));
    let was_running = !build.state.is_finished();
    let mut printed: Vec<(i64, u64)> = Vec::new();
    let mut attempt = build.attempt;
    loop {
        if build.attempt != attempt {
            attempt = build.attempt;
            printed.clear();
            println!("=== attempt {attempt}");
        }
        let tasks = store.tasks(build.id, build.attempt)?;
        let chosen: Vec<&Task> = tasks
            .iter()
            .filter(|t| match &task {
                Some(wanted) => t.name == *wanted || t.container == *wanted,
                None => !matches!(t.state, TaskState::Pending | TaskState::Skipped),
            })
            .collect();
        if chosen.is_empty() && task.is_some() && !follow {
            let names: Vec<&str> = tasks.iter().map(|t| t.name.as_str()).collect();
            bail!("no task {:?}; tasks are {}", task.unwrap_or_default(), names.join(", "));
        }
        for current in chosen {
            let from = printed.iter().find(|(o, _)| *o == current.ordinal).map(|(_, b)| *b);
            if from.is_none() {
                println!("{}", header(current));
            }
            let text = logs.read(build.id, build.attempt, current.ordinal, from.unwrap_or(0));
            let complete = text.rfind('\n').map_or(0, |end| end + 1);
            print!("{}", render(current, &text[..complete]));
            let offset = from.unwrap_or(0) + complete as u64;
            printed.retain(|(o, _)| *o != current.ordinal);
            printed.push((current.ordinal, offset));
        }
        if !follow || build.state.is_finished() {
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
        build = store.build(build.id)?.context("the build disappeared")?;
    }
    if follow && was_running {
        println!("{repo} {}", summary(&build));
    }
    Ok(if matches!(build.state, BuildState::Failed | BuildState::Errored) {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}
