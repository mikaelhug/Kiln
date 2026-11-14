use kiln_core::config::{self, Config, FILE_NAME, ReleaseMode};
use kiln_store::{AuditEntry, Build, BuildState, ImageRecord, Repo, Task, TaskKind, TaskState, Trigger};
use maud::{DOCTYPE, Markup, PreEscaped, html};

use super::super::checks::{duration, headline, phase_label, task_title};
use super::ansi;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub static ASSET_VERSION: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest([include_str!("assets/kiln.css"), include_str!("assets/kiln.js")].concat().as_bytes());
    hex::encode(&digest[..6])
});

#[derive(Debug, Clone)]
pub struct Viewer {
    pub login: String,
    pub avatar_url: String,
}

pub struct Context<'a> {
    pub viewer: &'a Viewer,
    pub github_web: &'a str,
    pub demo: bool,
}

pub enum Icon {
    Check,
    Cross,
    Spinner,
    Clock,
    Slash,
    Skip,
    Alert,
    Play,
    Pause,
    Rerun,
    Stop,
    Refresh,
    Trash,
    External,
    Tag,
    Branch,
    Pull,
    Hand,
}

pub fn icon(icon: Icon) -> Markup {
    let path = match icon {
        Icon::Check => r#"<path d="M4 10.5l3.5 3.5L16 6"/>"#,
        Icon::Cross => r#"<path d="M5.5 5.5l9 9M14.5 5.5l-9 9"/>"#,
        Icon::Spinner => r#"<path d="M10 3a7 7 0 1 1-7 7"/>"#,
        Icon::Clock => r#"<circle cx="10" cy="10" r="7"/><path d="M10 6v4l2.5 2"/>"#,
        Icon::Slash => r#"<circle cx="10" cy="10" r="7"/><path d="M5.5 14.5l9-9"/>"#,
        Icon::Skip => r#"<path d="M5 5l6 5-6 5zM14 5v10"/>"#,
        Icon::Alert => r#"<path d="M10 3l8 14H2z"/><path d="M10 8v4M10 14.5v.5"/>"#,
        Icon::Play => r#"<path d="M6 4l10 6-10 6z"/>"#,
        Icon::Pause => r#"<path d="M7 4v12M13 4v12"/>"#,
        Icon::Rerun => r#"<path d="M15.5 8A6 6 0 1 0 16 12"/><path d="M16 4v4h-4"/>"#,
        Icon::Stop => r#"<rect x="5" y="5" width="10" height="10" rx="1.5"/>"#,
        Icon::Refresh => {
            r#"<path d="M4 10a6 6 0 0 1 10.5-4L16 7.5M16 10a6 6 0 0 1-10.5 4L4 12.5"/><path d="M16 4v3.5h-3.5M4 16v-3.5h3.5"/>"#
        }
        Icon::Trash => r#"<path d="M4 6h12M8 6V4h4v2M6 6l1 10h6l1-10"/>"#,
        Icon::External => r#"<path d="M11 4h5v5M16 4l-7 7M14 12v4H4V6h4"/>"#,
        Icon::Tag => r#"<path d="M3 3h7l7 7-7 7-7-7z"/><circle cx="7" cy="7" r="1.2"/>"#,
        Icon::Branch => {
            r#"<circle cx="6" cy="5" r="2"/><circle cx="6" cy="15" r="2"/><circle cx="14" cy="8" r="2"/><path d="M6 7v6M14 10c0 2-2 3-8 3"/>"#
        }
        Icon::Pull => {
            r#"<circle cx="6" cy="5" r="2"/><circle cx="6" cy="15" r="2"/><circle cx="14" cy="15" r="2"/><path d="M6 7v6M14 13V8a2 2 0 0 0-2-2H9"/>"#
        }
        Icon::Hand => {
            r#"<path d="M7 10V5a1.5 1.5 0 0 1 3 0v4M10 9V4a1.5 1.5 0 0 1 3 0v6M13 9a1.5 1.5 0 0 1 3 0v3a5 5 0 0 1-5 5H9a5 5 0 0 1-4-2l-2-3a1.5 1.5 0 0 1 2.5-1.5L7 12"/>"#
        }
    };
    PreEscaped(format!(
        r#"<svg viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">{path}</svg>"#
    ))
}

const LOGO: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M4 21h16"/><path d="M6 21V11a6 6 0 0 1 12 0v10"/><path d="M12 17c-1.7 0-2.5-1.2-2.5-2.4 0-1.6 1.5-2.3 2.5-4.1 1 1.8 2.5 2.5 2.5 4.1 0 1.2-.8 2.4-2.5 2.4z"/></svg>"#;

pub fn kind(state: BuildState) -> &'static str {
    match state {
        BuildState::Succeeded => "ok",
        BuildState::Failed | BuildState::Errored => "bad",
        BuildState::Starting | BuildState::Running | BuildState::Releasing => "run",
        BuildState::Queued => "wait",
        BuildState::Cancelled | BuildState::Superseded | BuildState::Skipped => "wait",
    }
}

fn state_icon(state: BuildState) -> Icon {
    match state {
        BuildState::Succeeded => Icon::Check,
        BuildState::Failed | BuildState::Errored => Icon::Cross,
        BuildState::Starting | BuildState::Running | BuildState::Releasing => Icon::Spinner,
        BuildState::Queued => Icon::Clock,
        BuildState::Cancelled => Icon::Slash,
        BuildState::Superseded | BuildState::Skipped => Icon::Skip,
    }
}

pub fn state_label(state: BuildState) -> &'static str {
    match state {
        BuildState::Queued => "Queued",
        BuildState::Starting => "Starting",
        BuildState::Running => "Running",
        BuildState::Releasing => "Releasing",
        BuildState::Succeeded => "Passed",
        BuildState::Failed => "Failed",
        BuildState::Cancelled => "Cancelled",
        BuildState::Superseded => "Superseded",
        BuildState::Errored => "Error",
        BuildState::Skipped => "Skipped",
    }
}

fn task_kind(state: TaskState) -> (&'static str, Icon) {
    match state {
        TaskState::Passed => ("ok", Icon::Check),
        TaskState::Failed | TaskState::TimedOut => ("bad", Icon::Cross),
        TaskState::Running => ("run", Icon::Spinner),
        TaskState::Cancelled => ("wait", Icon::Slash),
        TaskState::Skipped => ("wait", Icon::Skip),
        TaskState::Pending => ("wait", Icon::Clock),
    }
}

pub fn time(seconds: i64) -> Markup {
    html! { time data-time=(seconds) { "" } }
}

pub fn build_duration(build: &Build) -> Markup {
    match (build.started_at, build.finished_at) {
        (Some(start), Some(end)) => html! { span class="duration" { (duration(end - start)) } },
        (Some(start), None) => html! { span class="duration" data-started=(start) { "" } },
        _ => html! {},
    }
}

pub fn trigger_pill(build: &Build) -> Markup {
    match build.trigger {
        Trigger::Push => html! { span class="pill" { (icon(Icon::Branch)) (build.ref_name) } },
        Trigger::Tag => html! { span class="pill accent" { (icon(Icon::Tag)) (build.ref_name) } },
        Trigger::PullRequest => {
            html! { span class="pill" { (icon(Icon::Pull)) "PR #" (build.pr_number.unwrap_or_default()) } }
        }
        Trigger::Manual => html! { span class="pill" { (icon(Icon::Hand)) (build.ref_name) } },
    }
}

pub fn repo_path(repo: &Repo) -> String {
    format!("/{}/{}", repo.owner, repo.name)
}

pub fn build_path(repo: &Repo, build: &Build) -> String {
    format!("/{}/{}/builds/{}", repo.owner, repo.name, build.number)
}

fn commit_title(build: &Build) -> String {
    let title = build.title();
    if title.is_empty() { format!("Commit {}", build.short_sha()) } else { title.to_owned() }
}

pub fn build_row(build: &Build, repo: Option<&Repo>, show_repo: bool) -> Markup {
    let href = repo.map(|r| build_path(r, build)).unwrap_or_default();
    html! {
        li class="row" {
            span class={"state " (kind(build.state))} title=(state_label(build.state)) { (icon(state_icon(build.state))) }
            div class="main" {
                a class="title" href=(href) { (commit_title(build)) }
                div class="meta" {
                    @if show_repo { @if let Some(repo) = repo { a href=(repo_path(repo)) { (repo.name) } } }
                    span { "#" (build.number) }
                    (trigger_pill(build))
                    code { (build.short_sha()) }
                    @if let Some(version) = &build.version {
                        span class={"pill " @if build.state == BuildState::Succeeded && build.detail.as_deref().is_some_and(|d| d.starts_with("Released")) { "ok" }} { (version) }
                    }
                    @if let Some(actor) = &build.actor { span { "by " (actor) } }
                    @if build.state.is_active() || build.state == BuildState::Queued { span { (headline(build)) } }
                }
            }
            div class="side" { (time(build.created_at)) (build_duration(build)) }
        }
    }
}

pub fn layout(title: &str, nav: &str, ctx: &Context<'_>, attrs: &[(&str, String)], content: Markup) -> Markup {
    let data: String = attrs.iter().map(|(k, v)| format!(" data-{k}=\"{}\"", html_escape(v))).collect();
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                title { (title) " · Kiln" }
                link rel="icon" href="/assets/favicon.svg" type="image/svg+xml";
                link rel="stylesheet" href={"/assets/kiln.css?v=" (*ASSET_VERSION)};
                script src={"/assets/kiln.js?v=" (*ASSET_VERSION)} defer {}
            }
            (PreEscaped(format!("<body{data}>")))
            header class="topbar" {
                div class="inner" {
                    a class="brand plain" href="/" { (PreEscaped(LOGO)) "Kiln" }
                    nav class="nav" {
                        a class=(if nav == "builds" { "active" } else { "" }) href="/" { "Builds" }
                        a class=(if nav == "repos" { "active" } else { "" }) href="/repos" { "Repositories" }
                        a class=(if nav == "system" { "active" } else { "" }) href="/system" { "System" }
                    }
                    div class="user" {
                        span class="live-dot" title="Live updates" {}
                        @if !ctx.viewer.avatar_url.is_empty() { img src=(ctx.viewer.avatar_url) alt=""; }
                        span { (ctx.viewer.login) }
                        form method="post" action="/logout" { button class="button" type="submit" { "Sign out" } }
                    }
                }
            }
            main { (content) }
            (PreEscaped("</body>"))
        }
    }
}

fn html_escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('"', "&quot;").replace('<', "&lt;").replace('>', "&gt;")
}

pub fn login(error: Option<&str>, demo: bool) -> Markup {
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                title { "Sign in · Kiln" }
                link rel="icon" href="/assets/favicon.svg" type="image/svg+xml";
                link rel="stylesheet" href={"/assets/kiln.css?v=" (*ASSET_VERSION)};
            }
            body {
                div class="login" {
                    div class="card" {
                        div class="brand" { (PreEscaped(LOGO)) "Kiln" }
                        p { "Self-hosted CI for your private repositories." }
                        @if let Some(error) = error {
                            div class="banner bad" { (icon(Icon::Alert)) div class="text" { (error) } }
                        }
                        a class="button primary plain" href="/auth/github" {
                            @if demo { "Enter the demo" } @else { "Sign in with GitHub" }
                        }
                    }
                }
            }
        }
    }
}

pub fn error_page(ctx: Option<&Context<'_>>, status: u16, message: &str) -> Markup {
    let content = html! {
        div class="card" {
            div class="empty" {
                h3 { (match status { 404 => "Not found", 403 => "Not allowed", 400 => "Something is off", _ => "Kiln hit an error" }) }
                p { (message) }
                a class="button" href="/" { "Back to builds" }
            }
        }
    };
    match ctx {
        Some(ctx) => layout("Error", "", ctx, &[], content),
        None => {
            html! { (DOCTYPE) html { head { link rel="stylesheet" href="/assets/kiln.css"; } body { main { (content) } } } }
        }
    }
}

pub type RepoSummary = (Repo, Option<Build>, Option<String>);

pub struct DashboardData {
    pub repos: Vec<Repo>,
    pub active: Vec<Build>,
    pub queued: Vec<Build>,
    pub recent: Vec<Build>,
    pub latest: Vec<RepoSummary>,
    pub slots: usize,
}

pub fn dashboard(ctx: &Context<'_>, data: &DashboardData) -> Markup {
    let enrolled = data.repos.iter().filter(|r| r.enrolled && r.removed_at.is_none()).count();
    layout(
        "Builds",
        "builds",
        ctx,
        &[("page", "dashboard".into())],
        html! {
            div class="page-head" {
                div {
                    h1 { "Builds" }
                    div class="sub" id="stats" {
                        span { (enrolled) " of " (data.repos.iter().filter(|r| r.removed_at.is_none()).count()) " repositories use Kiln" }
                        span { "·" }
                        span { (data.active.len()) " of " (data.slots) " build slots in use" }
                    }
                }
                div class="actions" {
                    form method="post" action="/system/refresh" {
                        button class="button" type="submit" { (icon(Icon::Refresh)) "Check GitHub now" }
                    }
                }
            }
            div id="live" { (dashboard_live(data)) }
        },
    )
}

pub fn dashboard_live(data: &DashboardData) -> Markup {
    let repo = |id: i64| data.repos.iter().find(|r| r.id == id);
    let enrolled_count = data.repos.iter().filter(|r| r.enrolled && r.removed_at.is_none()).count();
    let visible = data.repos.iter().filter(|r| r.removed_at.is_none()).count();
    let enrolled: Vec<&RepoSummary> = data.latest.iter().filter(|(r, _, _)| r.enrolled).collect();
    html! {
        template data-stats {
            span { (enrolled_count) " of " (visible) " repositories use Kiln" }
            span { "·" }
            span { (data.active.len()) " of " (data.slots) " build slots in use" }
        }
        div class="grid" {
            div class="stack" {
                @if !data.active.is_empty() {
                    section class="card" {
                        h2 { "Running" }
                        ul class="list" { @for build in &data.active { (build_row(build, repo(build.repo_id), true)) } }
                    }
                }
                @if !data.queued.is_empty() {
                    section class="card" {
                        h2 { "Waiting for a slot" }
                        ul class="list" { @for build in &data.queued { (build_row(build, repo(build.repo_id), true)) } }
                    }
                }
                section class="card" {
                    h2 { "Recent builds" }
                    @if data.recent.is_empty() {
                        (onboarding(enrolled.is_empty()))
                    } @else {
                        ul class="list" { @for build in &data.recent { (build_row(build, repo(build.repo_id), true)) } }
                    }
                }
            }
            aside class="stack" {
                section class="card" {
                    h2 { "Repositories" }
                    @if enrolled.is_empty() {
                        div class="empty" { p { "No repository has a " code { (FILE_NAME) } " yet." } a href="/repos" { "See all repositories" } }
                    } @else {
                        ul class="list" {
                            @for (repo, build, version) in &enrolled {
                                li class="row" {
                                    @match build {
                                        Some(build) => span class={"state " (kind(build.state))} title=(state_label(build.state)) { (icon(state_icon(build.state))) },
                                        None => span class="state wait" { (icon(Icon::Clock)) },
                                    }
                                    div class="main" {
                                        a class="title" href=(repo_path(repo)) { (repo.name) }
                                        div class="meta" {
                                            @if repo.paused { span class="pill warn" { "paused" } }
                                            @if repo.drift.is_some() { span class="pill warn" { "outside release" } }
                                            @if repo.config_error.is_some() { span class="pill bad" { "config error" } }
                                            @if let Some(version) = version { span class="pill" { (version) } }
                                            @if let Some(build) = build { (time(build.created_at)) }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn onboarding(no_repos: bool) -> Markup {
    html! {
        div class="empty" {
            @if no_repos {
                h3 { "Add Kiln to a repository" }
                p { "Commit a " code { (FILE_NAME) } " to the default branch of a repository the Kiln GitHub App can see. Kiln picks it up within a minute." }
                pre { "version = 1\n\n[test.unit]\nimage = \"node:26\"\nsteps = [\"npm ci\", \"npm test\"]" }
            } @else {
                h3 { "No builds yet" }
                p { "Builds appear here as soon as you push to a repository that uses Kiln." }
            }
        }
    }
}

pub fn repos(ctx: &Context<'_>, latest: &[RepoSummary]) -> Markup {
    layout(
        "Repositories",
        "repos",
        ctx,
        &[("page", "repos".into())],
        html! {
            div class="page-head" {
                div {
                    h1 { "Repositories" }
                    div class="sub" { span { "Every repository the Kiln GitHub App can see. A repository uses Kiln once its default branch has a " code { (FILE_NAME) } "." } }
                }
                div class="actions" {
                    form method="post" action="/system/refresh" { button class="button" type="submit" { (icon(Icon::Refresh)) "Check GitHub now" } }
                }
            }
            div id="live" { (repos_live(latest)) }
        },
    )
}

pub fn repos_live(latest: &[RepoSummary]) -> Markup {
    html! {
        section class="card" {
            @if latest.is_empty() {
                div class="empty" {
                    h3 { "No repositories yet" }
                    p { "Install the Kiln GitHub App on your account and choose which repositories it may see." }
                }
            } @else {
                table class="table" {
                    thead { tr { th { "Repository" } th { "Status" } th { "Default branch" } th { "Released" } } }
                    tbody {
                        @for (repo, build, version) in latest {
                            tr {
                                td { a href=(repo_path(repo)) { strong { (repo.name) } } div class="hint" { (repo.owner) } }
                                td {
                                    @if repo.removed_at.is_some() { span class="pill" { "no longer visible" } }
                                    @else if repo.paused { span class="pill warn" { "paused" } }
                                    @else if repo.config_error.is_some() { span class="pill bad" { "config error" } }
                                    @else if repo.enrolled { span class="pill ok" { "using Kiln" } }
                                    @else { span class="pill" { "no " (FILE_NAME) } }
                                }
                                td {
                                    @match build {
                                        Some(build) => { span class={"pill " (kind(build.state))} { (state_label(build.state)) } " " (time(build.created_at)) },
                                        None => span class="hint" { "never" },
                                    }
                                }
                                td { @match version { Some(version) => code { (version) }, None => span class="hint" { "–" } } }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Show {
    All,
    Branch,
    Pulls,
    Tags,
}

impl Show {
    pub fn parse(value: Option<&str>) -> Self {
        match value {
            Some("branch") => Show::Branch,
            Some("pulls") => Show::Pulls,
            Some("tags") => Show::Tags,
            _ => Show::All,
        }
    }

    pub fn triggers(self) -> Vec<Trigger> {
        match self {
            Show::All => Vec::new(),
            Show::Branch => vec![Trigger::Push, Trigger::Manual],
            Show::Pulls => vec![Trigger::PullRequest],
            Show::Tags => vec![Trigger::Tag],
        }
    }

    fn query(self) -> &'static str {
        match self {
            Show::All => "",
            Show::Branch => "?show=branch",
            Show::Pulls => "?show=pulls",
            Show::Tags => "?show=tags",
        }
    }
}

pub struct RepoData {
    pub show: Show,
    pub repo: Repo,
    pub builds: Vec<Build>,
    pub releases: Vec<(String, i64, i64)>,
    pub config: Option<Config>,
    pub more: Option<i64>,
    pub busy: bool,
}

pub fn repo(ctx: &Context<'_>, data: &RepoData) -> Markup {
    let repo = &data.repo;
    layout(
        &repo.name,
        "repos",
        ctx,
        &[("page", "repo".into()), ("repo", repo.id.to_string())],
        html! {
            div id="live" { (repo_live(ctx, data)) }
        },
    )
}

pub fn repo_live(ctx: &Context<'_>, data: &RepoData) -> Markup {
    let repo = &data.repo;
    let github = format!("{}/{}", ctx.github_web, repo.full_name());
    html! {
        div class="crumbs" { a href="/repos" { "Repositories" } " / " (repo.owner) }
        div class="page-head" {
            div {
                h1 { (repo.name) }
                div class="sub" {
                    span class="pill" { (icon(Icon::Branch)) (repo.default_branch) }
                    @if let Some(head) = &repo.head_sha { code { (&head[..head.len().min(7)]) } }
                    a href=(github) target="_blank" rel="noopener" { "GitHub " (icon(Icon::External)) }
                }
            }
            div class="actions" {
                @if repo.enrolled && !repo.paused {
                    form method="post" action={(repo_path(repo)) "/build"} {
                        button class="button primary" type="submit" { (icon(Icon::Play)) "Build " (repo.default_branch) }
                    }
                }
                @if repo.paused {
                    form method="post" action={(repo_path(repo)) "/resume"} { button class="button" type="submit" { (icon(Icon::Play)) "Resume" } }
                } @else if repo.enrolled {
                    form method="post" action={(repo_path(repo)) "/pause"} data-confirm="Pause Kiln for this repository? Pushes are ignored until you resume." {
                        button class="button" type="submit" { (icon(Icon::Pause)) "Pause" }
                    }
                }
                @if repo.enrolled {
                    form method="post" action={(repo_path(repo)) "/reset-cache"} data-confirm="Delete the build cache for this repository? The next build starts cold." {
                        button class="button danger" type="submit" disabled[data.busy] title=(if data.busy { "Wait until the running build finishes" } else { "" }) { (icon(Icon::Trash)) "Reset cache" }
                    }
                }
            }
        }
        @if repo.paused {
            div class="banner warn" { (icon(Icon::Pause)) div class="text" { strong { "Paused" } "Kiln ignores new pushes, tags and pull requests here until you resume." } }
        }
        @if let Some(drift) = &repo.drift {
            div class="banner warn" {
                (icon(Icon::Alert))
                div class="text" {
                    strong { "Published outside Kiln" }
                    "These versions appeared in the registry without a Kiln release: " code { (drift) } ". If another CI still publishes this repository, remove it; otherwise check who holds the registry credential."
                    form method="post" action={(repo_path(repo)) "/dismiss-drift"} style="margin-top:8px" { button class="button" type="submit" { "Dismiss" } }
                }
            }
        }
        @if let Some(error) = &repo.config_error {
            div class="banner bad" { (icon(Icon::Alert)) div class="text" { strong { (FILE_NAME) " on " (repo.default_branch) " has errors" } pre { (error) } } }
        }
        @if !repo.enrolled && repo.removed_at.is_none() {
            div class="banner wait" { (icon(Icon::Clock)) div class="text" { strong { "Not using Kiln yet" } "Commit a " code { (FILE_NAME) } " to " (repo.default_branch) " to start building this repository." } }
        }
        div class="grid" {
            section class="card" {
                div class="tabs" {
                    @for (show, label) in [(Show::All, "All builds"), (Show::Branch, (repo.default_branch.as_str())), (Show::Pulls, "Pull requests"), (Show::Tags, "Tags")] {
                        a class=(if show == data.show { "tab active" } else { "tab" }) href={(repo_path(repo)) (show.query())} { (label) }
                    }
                }
                @if data.builds.is_empty() {
                    div class="empty" { p { (match data.show { Show::All => "No builds yet.", Show::Branch => "No builds of the default branch yet.", Show::Pulls => "No pull request builds yet.", Show::Tags => "No tag builds yet." }) } }
                } @else {
                    ul class="list" { @for build in &data.builds { (build_row(build, Some(repo), false)) } }
                    @if let Some(before) = data.more {
                        @let separator = if data.show == Show::All { "?" } else { "&" };
                        a class="more" href={(repo_path(repo)) (data.show.query()) (separator) "before=" (before)} { "Older builds" }
                    }
                }
            }
            aside class="stack" {
                section class="card" {
                    h2 { "Releases" }
                    @if data.releases.is_empty() {
                        div class="empty" { p { "Nothing released yet." } }
                    } @else {
                        @for (version, number, at) in &data.releases {
                            div class="release" {
                                span class="pill ok" { (version) }
                                a href={(repo_path(repo)) "/builds/" (number)} { "#" (number) }
                                span class="hint" { (time(*at)) }
                            }
                        }
                    }
                }
                @if let Some(config) = &data.config {
                    section class="card" {
                        h2 { "Configuration" }
                        dl class="facts" {
                            dt { "Images" } dd { @for (i, image) in config.images.iter().enumerate() { @if i > 0 { ", " } code { (image.name) } } }
                            dt { "Releases" } dd { (match config.release.mode { ReleaseMode::Rc => "every push to the default branch, and tags", ReleaseMode::Tags => "tags only" }) @if config.release.shadow { " · shadow mode" } }
                            dt { "Tests" } dd { @if config.test.is_empty() { "none" } @else { (config.test.keys().cloned().collect::<Vec<_>>().join(", ")) } }
                            dt { "Verify" } dd { @if config.verify.is_empty() { "none" } @else { (config.verify.keys().cloned().collect::<Vec<_>>().join(", ")) } }
                            dt { "Runs on" } dd { (triggers(&config.on)) }
                        }
                    }
                }
            }
        }
    }
}

fn triggers(on: &config::Triggers) -> String {
    let mut parts = Vec::new();
    if on.push {
        parts.push("pushes")
    }
    if on.tags {
        parts.push("tags")
    }
    if on.pull_request {
        parts.push("pull requests")
    }
    if parts.is_empty() { "nothing".into() } else { parts.join(", ") }
}

pub struct BuildData {
    pub ahead: Option<usize>,
    pub repo: Repo,
    pub build: Build,
    pub attempt: i64,
    pub tasks: Vec<Task>,
    pub images: Vec<ImageRecord>,
    pub rerun_of: Option<Build>,
}

pub fn build(ctx: &Context<'_>, data: &BuildData) -> Markup {
    let selected = default_task(&data.tasks);
    layout(
        &format!("#{} {}", data.build.number, data.repo.name),
        "builds",
        ctx,
        &[
            ("page", "build".into()),
            ("build", data.build.id.to_string()),
            ("repo", data.repo.id.to_string()),
            ("attempt", data.attempt.to_string()),
        ],
        html! {
            div id="live" { (build_head(ctx, data)) }
            div class="build-layout" {
                aside id="tasks" class="tasks" { (tasks_card(data, selected)) }
                div class="stack" {
                    div id="log" {
                        @if data.tasks.is_empty() {
                            div class="log" { div class="log-empty" { (no_tasks_message(&data.build)) } }
                        } @else {
                            div class="log" { div class="log-empty" { "Loading log…" } }
                        }
                    }
                    @if !data.images.is_empty() { div id="images" { (images_card(&data.images)) } }
                }
            }
        },
    )
}

fn no_tasks_message(build: &Build) -> &'static str {
    match build.state {
        BuildState::Queued => "This build is waiting for a free build slot.",
        BuildState::Skipped => "This build was skipped.",
        BuildState::Failed if build.failure == Some(kiln_store::Failure::Config) => {
            "Kiln could not read the configuration, so nothing ran."
        }
        _ if build.trigger == Trigger::Tag => "This release reused an image that was already built and verified.",
        _ => "Nothing ran for this build.",
    }
}

pub fn default_task(tasks: &[Task]) -> Option<i64> {
    tasks
        .iter()
        .find(|t| matches!(t.state, TaskState::Failed | TaskState::TimedOut))
        .or_else(|| tasks.iter().find(|t| t.state == TaskState::Running))
        .or_else(|| tasks.iter().rev().find(|t| t.state != TaskState::Pending && t.state != TaskState::Skipped))
        .map(|t| t.ordinal)
}

pub fn build_head(ctx: &Context<'_>, data: &BuildData) -> Markup {
    let BuildData { repo, build, .. } = data;
    let commit_url = format!("{}/{}/commit/{}", ctx.github_web, repo.full_name(), build.sha);
    let path = build_path(repo, build);
    let released_tag = build.trigger == Trigger::Tag && build.state == BuildState::Succeeded;
    html! {
        div class="crumbs" { a href=(repo_path(repo)) { (repo.full_name()) } " / #" (build.number) }
        div class="page-head" {
            div {
                h1 { (commit_title(build)) }
                div class="sub" {
                    span class={"pill " (kind(build.state))} { (state_label(build.state)) }
                    a class="plain" href=(trigger_url(ctx, repo, build)) target="_blank" rel="noopener" { (trigger_pill(build)) }
                    a href=(commit_url) target="_blank" rel="noopener" { code { (build.short_sha()) } }
                    @if let Some(version) = &build.version { span class="pill" { (icon(Icon::Tag)) (version) } }
                    @if let Some(actor) = &build.actor { span { "started by " (actor) } }
                    span { (time(build.created_at)) }
                    (build_duration(build))
                    @if let Some(started) = build.started_at.filter(|s| s - build.created_at >= 10) {
                        span { "waited " (duration(started - build.created_at)) " for a slot" }
                    }
                    @if let Some(original) = &data.rerun_of { span { "rerun of " a href=(build_path(repo, original)) { "#" (original.number) } } }
                }
            }
            div class="actions" {
                @if build.state == BuildState::Queued || build.state == BuildState::Starting || build.state == BuildState::Running {
                    form method="post" action={(path) "/cancel"} data-confirm="Cancel this build?" {
                        button class="button danger" type="submit" { (icon(Icon::Stop)) "Cancel" }
                    }
                }
                @if build.state.is_finished() && !released_tag {
                    form method="post" action={(path) "/rerun"} {
                        button class="button" type="submit" { (icon(Icon::Rerun)) "Rerun" }
                    }
                }
            }
        }
        (banner(build, data.ahead))
        @if build.attempt > 1 {
            div class="sub hint" style="margin:-8px 0 16px" {
                "Attempt " (data.attempt) " of " (build.attempt) " · "
                @for attempt in 1..=build.attempt {
                    @if attempt == data.attempt { strong { (attempt) } } @else { a href={(path) "?attempt=" (attempt)} { (attempt) } } " "
                }
            }
        }
    }
}

fn trigger_url(ctx: &Context<'_>, repo: &Repo, build: &Build) -> String {
    let base = format!("{}/{}", ctx.github_web, repo.full_name());
    match build.trigger {
        Trigger::PullRequest => format!("{base}/pull/{}", build.pr_number.unwrap_or_default()),
        Trigger::Tag => format!("{base}/tree/{}", build.ref_name),
        Trigger::Push | Trigger::Manual => format!("{base}/tree/{}", build.ref_name),
    }
}

fn banner(build: &Build, ahead: Option<usize>) -> Markup {
    let class = kind(build.state);
    let detail = build.detail.clone().unwrap_or_default();
    let config = build.failure == Some(kiln_store::Failure::Config);
    let title = headline(build);
    let show_detail = !detail.is_empty() && !title.contains(detail.lines().next().unwrap_or_default()) || config;
    html! {
        div class={"banner " (class)} {
            (icon(state_icon(build.state)))
            div class="text" {
                strong { (title) }
                @if show_detail {
                    @if config || detail.contains('\n') { pre { (detail) } } @else { (detail) }
                }
                @if build.state.is_active() && build.state != BuildState::Releasing && title != phase_label(build.phase) { span class="hint" { (phase_label(build.phase)) } }
                @if let Some(ahead) = ahead {
                    span class="hint" { (match ahead { 0 => "It starts as soon as a slot is free.".to_owned(), 1 => "1 build is ahead of it.".to_owned(), n => format!("{n} builds are ahead of it.") }) }
                }
            }
        }
    }
}

pub fn tasks_card(data: &BuildData, selected: Option<i64>) -> Markup {
    html! {
        section class="card" {
            @if data.tasks.is_empty() {
                div class="empty" { p { "No tasks." } }
            }
            @for (index, task) in data.tasks.iter().enumerate() {
                @let phase = phase_group(task.kind);
                @if index == 0 || phase_group(data.tasks[index - 1].kind) != phase {
                    div class="phase-label" { (phase) }
                }
                @let (class, glyph) = task_kind(task.state);
                a class={"task" @if Some(task.ordinal) == selected { " selected" }} href={"#task-" (task.ordinal)}
                    data-task=(task.ordinal) data-attempt=(data.attempt) data-state=(task.state.as_str()) data-default[Some(task.ordinal) == selected] {
                    span class={"state " (class)} { (icon(glyph)) }
                    span class="name" { (task_title(task)) }
                    span class="time" {
                        @match (task.started_at, task.finished_at) {
                            (Some(start), Some(end)) => (duration(end - start)),
                            (Some(start), None) => span data-started=(start) { "" },
                            _ => "",
                        }
                    }
                }
            }
            @if data.images.iter().any(|i| i.released.is_some()) || data.build.state == BuildState::Releasing {
                div class="phase-label" { "Release" }
                @for image in &data.images {
                    @let done = image.released.is_some();
                    div class="task" {
                        span class={"state " (if done { "ok" } else { "run" })} { (icon(if done { Icon::Check } else { Icon::Spinner })) }
                        span class="name" { (image.name) }
                        span class="time" { @if let Some(tag) = &image.released { (tag) } }
                    }
                }
            }
        }
    }
}

fn phase_group(kind: TaskKind) -> &'static str {
    match kind {
        TaskKind::Fetch => "Source",
        TaskKind::Test => "Test",
        TaskKind::Image | TaskKind::Push => "Build",
        TaskKind::Verify => "Verify",
    }
}

pub fn images_card(images: &[ImageRecord]) -> Markup {
    html! {
        section class="card" {
            h2 { "Images" }
            table class="table" {
                thead { tr { th { "Image" } th { "Digest" } th { "Released as" } } }
                tbody {
                    @for image in images {
                        tr {
                            td { strong { (image.name) } }
                            td { @match &image.digest { Some(digest) => code title=(digest) { (&digest[..digest.len().min(19)]) "…" }, None => span class="hint" { "not built" } } }
                            td { @match &image.released { Some(tag) => span class="pill ok" { (tag) }, None => span class="hint" { "–" } } }
                        }
                    }
                }
            }
        }
    }
}

pub fn log_view(build: &Build, attempt: i64, task: &Task, text: &str) -> Markup {
    let raw = format!("/builds/{}/logs/{attempt}/{}.txt", build.id, task.ordinal);
    let (class, _) = task_kind(task.state);
    html! {
        div class="log" {
            div class="log-head" data-bytes=(text.len()) data-state=(task.state.as_str()) {
                span class={"pill " (class)} { (match task.state { TaskState::Passed => "passed", TaskState::Failed => "failed", TaskState::TimedOut => "timed out", TaskState::Running => "running", TaskState::Cancelled => "cancelled", TaskState::Skipped => "skipped", TaskState::Pending => "waiting" }) }
                span class="name" { (task_title(task)) }
                @if let Some(code) = task.exit_code.filter(|c| *c != 0) { span { "exit " (code) } }
                @if task.state == TaskState::Running { button type="button" data-follow { "Follow" } }
                a href=(raw) download { "Raw log" }
            }
            div class="log-body" {
                @if text.trim().is_empty() {
                    div class="log-empty" { @if task.state == TaskState::Pending { "This task has not started yet." } @else if task.state == TaskState::Skipped { "This task was skipped because an earlier task failed." } @else { "No output yet." } }
                } @else {
                    (render_log(task, text))
                }
            }
        }
    }
}

fn render_log(task: &Task, text: &str) -> Markup {
    let mut preamble = Vec::new();
    let mut sections: Vec<(usize, Vec<&str>)> = Vec::new();
    for line in text.lines() {
        match kiln_runner::parse_marker(line) {
            Some(kiln_runner::Marker::StepStart { index, .. }) => sections.push((index, Vec::new())),
            Some(_) => {}
            None => match sections.last_mut() {
                Some((_, lines)) => lines.push(line),
                None => preamble.push(line),
            },
        }
    }
    html! {
        @if !preamble.is_empty() || sections.is_empty() {
            div class="lines" { @for line in &preamble { (ansi::line(line)) } }
        }
        @for (index, lines) in &sections {
            @let step = task.steps.iter().find(|s| s.ordinal == *index as i64);
            @let state = step.map(|s| s.state).unwrap_or(TaskState::Running);
            @let open = matches!(state, TaskState::Running | TaskState::Failed | TaskState::TimedOut);
            details open[open] data-open-last[*index + 1 == sections.len() && open] {
                summary {
                    span class={"dot " (task_kind(state).0)} {}
                    span class="name" { (step.map(|s| s.name.clone()).unwrap_or_else(|| format!("Step {}", index + 1))) }
                    span class="time" {
                        @if let Some(step) = step {
                            @match (step.started_at, step.finished_at) {
                                (Some(start), Some(end)) => (duration(end - start)),
                                (Some(start), None) => span data-started=(start) { "" },
                                _ => "",
                            }
                        }
                    }
                }
                div class="lines" { @for line in lines { (ansi::line(line)) } }
            }
        }
    }
}

pub fn system(ctx: &Context<'_>, facts: &[(&str, String)], audit: &[(AuditEntry, Option<String>)]) -> Markup {
    layout(
        "System",
        "system",
        ctx,
        &[("page", "system".into())],
        html! {
            div class="page-head" {
                div { h1 { "System" } div class="sub" { "Kiln " (VERSION) @if ctx.demo { " · demo mode" } } }
                div class="actions" {
                    form method="post" action="/system/refresh" { button class="button" type="submit" { (icon(Icon::Refresh)) "Check GitHub now" } }
                }
            }
            div class="grid" {
                section class="card" {
                    h2 { "Activity" }
                    @if audit.is_empty() {
                        div class="empty" { p { "Nothing recorded yet." } }
                    } @else {
                        table class="table" {
                            thead { tr { th { "When" } th { "Who" } th { "What" } } }
                            tbody {
                                @for (entry, link) in audit {
                                    tr {
                                        td class="when" { (time(entry.at)) }
                                        td { (entry.actor) }
                                        td {
                                            @match link {
                                                Some(link) => a href=(link) { (entry.action.replace('_', " ")) },
                                                None => (entry.action.replace('_', " ")),
                                            }
                                            @if let Some(detail) = &entry.detail { " " span class="hint" { (detail) } }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                aside class="card" {
                    h2 { "Configuration" }
                    dl class="facts" { @for (key, value) in facts { dt { (key) } dd { (value) } } }
                }
            }
        },
    )
}
