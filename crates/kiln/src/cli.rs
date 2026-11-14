use std::path::Path;
use std::process::{Command, ExitCode};

use anyhow::{Context, Result, bail};
use kiln_core::config::{self, Config, Diagnostic, ReleaseMode};
use kiln_core::version::{self, Allocation, Publish};

#[derive(Clone, Debug)]
pub enum Event {
    Push,
    Tag(String),
    PullRequest,
}

pub fn parse_event(value: &str) -> Result<Event, String> {
    match value {
        "push" => Ok(Event::Push),
        "pr" | "pull_request" => Ok(Event::PullRequest),
        _ => value
            .strip_prefix("tag:")
            .map(|tag| Event::Tag(tag.to_owned()))
            .ok_or_else(|| "use push, pr or tag:<name>".to_owned()),
    }
}

pub fn schema() -> Result<ExitCode> {
    println!("{}", serde_json::to_string_pretty(&config::json_schema())?);
    Ok(ExitCode::SUCCESS)
}

fn directory(file: &Path) -> &Path {
    file.parent().filter(|dir| !dir.as_os_str().is_empty()).unwrap_or(Path::new("."))
}

fn repo_name(file: &Path, repo: Option<String>) -> Result<String> {
    if let Some(repo) = repo {
        return Ok(repo);
    }
    let dir = directory(file).canonicalize().with_context(|| format!("resolving {}", file.display()))?;
    dir.file_name()
        .and_then(|name| name.to_str())
        .map(str::to_owned)
        .context("cannot infer the repository name, pass --repo")
}

fn load(file: &Path, repo: Option<String>) -> Result<Result<Config, Vec<Diagnostic>>> {
    let text = std::fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?;
    Ok(config::parse(&text, &repo_name(file, repo)?))
}

pub fn validate(file: &Path, repo: Option<String>, json: bool) -> Result<ExitCode> {
    let diagnostics = load(file, repo)?.err().unwrap_or_default();
    if json {
        let rows: Vec<_> = diagnostics
            .iter()
            .map(|d| serde_json::json!({ "path": d.path, "line": d.line, "message": d.message }))
            .collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
    } else if diagnostics.is_empty() {
        println!("{} is valid", file.display());
    } else {
        for diagnostic in &diagnostics {
            println!("{diagnostic}");
        }
    }
    Ok(if diagnostics.is_empty() { ExitCode::SUCCESS } else { ExitCode::FAILURE })
}

pub fn plan(file: &Path, repo: Option<String>, event: &Event, published: &[String]) -> Result<ExitCode> {
    let config = match load(file, repo)? {
        Ok(config) => config,
        Err(diagnostics) => {
            for diagnostic in &diagnostics {
                eprintln!("{diagnostic}");
            }
            return Ok(ExitCode::FAILURE);
        }
    };
    let enabled = match event {
        Event::Push => config.on.push,
        Event::Tag(_) => config.on.tags,
        Event::PullRequest => config.on.pull_request,
    };
    if !enabled {
        println!("nothing runs: this event is turned off in [on]");
        return Ok(ExitCode::SUCCESS);
    }
    for (name, job) in &config.test {
        println!("test    {name:<16} {} ({} steps)", job.image, job.steps.len());
    }
    if matches!(event, Event::PullRequest) {
        println!("pull requests run tests only");
        return Ok(ExitCode::SUCCESS);
    }
    for image in &config.images {
        println!("build   {:<16} {} in {}", image.name, image.file, image.context);
    }
    for (name, job) in &config.verify {
        println!("verify  {name:<16} {}", job.image);
    }
    let allocation = match event {
        Event::Tag(tag) => version::tag_version(tag)?,
        Event::Push if config.release.mode == ReleaseMode::Tags => {
            println!("release none: \"tags\" mode only publishes tag pushes");
            return Ok(ExitCode::SUCCESS);
        }
        _ => version::next_rc(&git_tags(file)?, published, 0),
    };
    report(&config, &allocation, published)
}

fn report(config: &Config, allocation: &Allocation, published: &[String]) -> Result<ExitCode> {
    let order = config.images.iter().map(|image| image.name.as_str()).collect::<Vec<_>>().join(", ");
    let shadow = if config.release.shadow { " (shadow, not published)" } else { "" };
    let version = &allocation.version;
    match version::assess(version, published)? {
        Publish::Deploys => println!("release {version} -> {order}{shadow}"),
        Publish::AlreadyPublished => println!("release {version} is already published"),
        Publish::BelowCurrent { current } => {
            println!("release {version} -> {order}{shadow}; Flux keeps {current} because it is higher")
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn git_tags(file: &Path) -> Result<Vec<String>> {
    let output = Command::new("git").args(["tag", "--list", "v*"]).current_dir(directory(file)).output()?;
    if !output.status.success() {
        bail!("git tag failed: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(String::from_utf8(output.stdout)?.lines().map(str::to_owned).collect())
}
