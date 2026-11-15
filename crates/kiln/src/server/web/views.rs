use kiln_core::config::{self, Config, FILE_NAME, ReleaseMode};
use kiln_store::{AuditEntry, Build, BuildState, Failure, ImageRecord, Repo, Task, TaskKind, TaskState, Trigger};
use maud::{DOCTYPE, Markup, PreEscaped, html};

use super::super::checks::{duration, headline, phase_label, task_title};
use super::ansi;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub static ASSET_VERSION: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(
        [include_str!("assets/kiln.css"), include_str!("assets/kiln.js"), include_str!("assets/connect.js")]
            .concat()
            .as_bytes(),
    );
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
    pub registry_note: Option<String>,
}

#[derive(Clone, Copy)]
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
    Search,
    Box,
    Activity,
    Book,
    Server,
    Copy,
    Chevron,
    LogOut,
    Wrap,
    Expand,
    Download,
    ArrowDown,
    Timer,
    Commit,
    User,
    More,
    Gauge,
    Layers,
    Rocket,
    Cog,
    Key,
    Users,
}

pub fn icon(icon: Icon) -> Markup {
    let path = match icon {
        Icon::Check => r#"<path d="M4.5 10.5l3.5 3.5 7.5-8"/>"#,
        Icon::Cross => r#"<path d="M6 6l8 8M14 6l-8 8"/>"#,
        Icon::Spinner => r#"<path d="M10 2.5a7.5 7.5 0 0 1 7.5 7.5"/>"#,
        Icon::Clock => r#"<circle cx="10" cy="10" r="7"/><path d="M10 6v4l2.5 2"/>"#,
        Icon::Slash => r#"<circle cx="10" cy="10" r="7"/><path d="M5.5 14.5l9-9"/>"#,
        Icon::Skip => r#"<path d="M5 10h10"/>"#,
        Icon::Alert => r#"<path d="M10 3l8 14H2z"/><path d="M10 8.5v3.5M10 14.5v.01"/>"#,
        Icon::Play => r#"<path d="M6.5 4.5l9 5.5-9 5.5z"/>"#,
        Icon::Pause => r#"<path d="M7 4.5v11M13 4.5v11"/>"#,
        Icon::Rerun => r#"<path d="M15.5 8A6 6 0 1 0 16 12"/><path d="M16 4v4h-4"/>"#,
        Icon::Stop => r#"<rect x="5.5" y="5.5" width="9" height="9" rx="1.5"/>"#,
        Icon::Refresh => {
            r#"<path d="M4 10a6 6 0 0 1 10.5-4L16 7.5M16 10a6 6 0 0 1-10.5 4L4 12.5"/><path d="M16 4v3.5h-3.5M4 16v-3.5h3.5"/>"#
        }
        Icon::Trash => r#"<path d="M4 6h12M8 6V4h4v2M6 6l1 10h6l1-10"/>"#,
        Icon::External => r#"<path d="M11 4h5v5M16 4l-7 7M14 12v4H4V6h4"/>"#,
        Icon::Tag => r#"<path d="M3 3h7l7 7-7 7-7-7z"/><circle cx="7" cy="7" r="1.2"/>"#,
        Icon::Branch => {
            r#"<circle cx="6" cy="5" r="2"/><circle cx="6" cy="15" r="2"/><circle cx="14" cy="7" r="2"/><path d="M6 7v6M14 9c0 2.5-2 3.5-8 4"/>"#
        }
        Icon::Pull => {
            r#"<circle cx="6" cy="5" r="2"/><circle cx="6" cy="15" r="2"/><circle cx="14" cy="15" r="2"/><path d="M6 7v6M14 13V8a2 2 0 0 0-2-2H9"/><path d="M10.5 4L9 6l1.5 2"/>"#
        }
        Icon::Search => r#"<circle cx="9" cy="9" r="5.5"/><path d="M13 13l4 4"/>"#,
        Icon::Box => r#"<path d="M3 6.5L10 3l7 3.5v7L10 17l-7-3.5z"/><path d="M3 6.5L10 10l7-3.5M10 10v7"/>"#,
        Icon::Activity => r#"<path d="M2 10h3.5l2-5 4 10 2-5H18"/>"#,
        Icon::Book => {
            r#"<path d="M4 15.5V4.5A1.5 1.5 0 0 1 5.5 3H16v11.5H5.5A1.5 1.5 0 0 0 4 16v0a1.5 1.5 0 0 0 1.5 1.5H16"/>"#
        }
        Icon::Server => {
            r#"<rect x="3" y="3.5" width="14" height="5.5" rx="1.5"/><rect x="3" y="11" width="14" height="5.5" rx="1.5"/><path d="M6.5 6.25h.01M6.5 13.75h.01"/>"#
        }
        Icon::Copy => {
            r#"<rect x="7" y="7" width="10" height="10" rx="2"/><path d="M13 7V5a2 2 0 0 0-2-2H5a2 2 0 0 0-2 2v6a2 2 0 0 0 2 2h2"/>"#
        }
        Icon::Chevron => r#"<path d="M8 5l5 5-5 5"/>"#,
        Icon::LogOut => r#"<path d="M8 17H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h3M13 14l4-4-4-4M17 10H8"/>"#,
        Icon::Wrap => r#"<path d="M3 5h14M3 10h11a3 3 0 0 1 0 6h-4M3 15h4"/><path d="M12 14l-2 2 2 2"/>"#,
        Icon::Expand => r#"<path d="M6 8l4-4 4 4M6 12l4 4 4-4"/>"#,
        Icon::Download => r#"<path d="M10 3v10M6 9l4 4 4-4M4 17h12"/>"#,
        Icon::ArrowDown => r#"<path d="M10 4v12M5 11l5 5 5-5"/>"#,
        Icon::Timer => r#"<circle cx="10" cy="11" r="6"/><path d="M10 8v3l2 1M8 2.5h4"/>"#,
        Icon::Commit => r#"<circle cx="10" cy="10" r="3"/><path d="M2 10h5M13 10h5"/>"#,
        Icon::User => r#"<circle cx="10" cy="7" r="3"/><path d="M4 17a6 6 0 0 1 12 0"/>"#,
        Icon::More => r#"<path d="M5 10h.01M10 10h.01M15 10h.01" stroke-width="2.6"/>"#,
        Icon::Gauge => r#"<path d="M3.5 14a7 7 0 1 1 13 0"/><path d="M10 11l3-4"/>"#,
        Icon::Layers => r#"<path d="M10 3l7 4-7 4-7-4z"/><path d="M3 11l7 4 7-4"/>"#,
        Icon::Cog => {
            r#"<circle cx="10" cy="10" r="2.5"/><path d="M10 2.5v2M10 15.5v2M2.5 10h2M15.5 10h2M4.7 4.7l1.4 1.4M13.9 13.9l1.4 1.4M4.7 15.3l1.4-1.4M13.9 6.1l1.4-1.4"/>"#
        }
        Icon::Key => r#"<circle cx="7" cy="13" r="3.5"/><path d="M9.5 10.5L16 4M13.5 6.5l2 2M12 8l1.5 1.5"/>"#,
        Icon::Users => {
            r#"<circle cx="7.5" cy="7" r="2.8"/><path d="M2.5 16.5a5 5 0 0 1 10 0"/><path d="M13 4.5a2.8 2.8 0 0 1 0 5.2M14.5 12.2a5 5 0 0 1 3 4.3"/>"#
        }
        Icon::Rocket => {
            r#"<path d="M11.5 14.5L8 11l1-3c1.5-3.5 4-5 8-5 0 4-1.5 6.5-5 8z"/><path d="M8 11l-3 .5L3 9.5l3-1.5M9 12l-.5 3 2 2 1.5-3"/><circle cx="13" cy="7" r="1"/>"#
        }
    };
    PreEscaped(format!(
        r#"<svg viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">{path}</svg>"#
    ))
}

const LOGO: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M4 21h16"/><path d="M6 21V11a6 6 0 0 1 12 0v10"/><path d="M12 17c-1.7 0-2.5-1.2-2.5-2.4 0-1.6 1.5-2.3 2.5-4.1 1 1.8 2.5 2.5 2.5 4.1 0 1.2-.8 2.4-2.5 2.4z"/></svg>"#;

const GITHUB: &str = r#"<svg viewBox="0 0 16 16" fill="currentColor" aria-hidden="true"><path d="M8 0C3.58 0 0 3.58 0 8c0 3.54 2.29 6.53 5.47 7.59.4.07.55-.17.55-.38 0-.19-.01-.82-.01-1.49-2.01.37-2.53-.49-2.69-.94-.09-.23-.48-.94-.82-1.13-.28-.15-.68-.52-.01-.53.63-.01 1.08.58 1.23.82.72 1.21 1.87.87 2.33.66.07-.52.28-.87.51-1.07-1.78-.2-3.64-.89-3.64-3.95 0-.87.31-1.59.82-2.15-.08-.2-.36-1.02.08-2.12 0 0 .67-.21 2.2.82.64-.18 1.32-.27 2-.27.68 0 1.36.09 2 .27 1.53-1.04 2.2-.82 2.2-.82.44 1.1.16 1.92.08 2.12.51.56.82 1.27.82 2.15 0 3.07-1.87 3.75-3.65 3.95.29.25.54.73.54 1.48 0 1.07-.01 1.93-.01 2.2 0 .21.15.46.55.38A8.01 8.01 0 0 0 16 8c0-4.42-3.58-8-8-8z"/></svg>"#;

pub fn kind(state: BuildState) -> &'static str {
    match state {
        BuildState::Succeeded => "ok",
        BuildState::Failed | BuildState::Errored => "bad",
        BuildState::Starting | BuildState::Running | BuildState::Releasing => "run",
        BuildState::Queued | BuildState::Cancelled | BuildState::Superseded | BuildState::Skipped => "wait",
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

fn task_label(state: TaskState) -> &'static str {
    match state {
        TaskState::Passed => "Passed",
        TaskState::Failed => "Failed",
        TaskState::TimedOut => "Timed out",
        TaskState::Running => "Running",
        TaskState::Cancelled => "Cancelled",
        TaskState::Skipped => "Skipped",
        TaskState::Pending => "Waiting",
    }
}

fn status(state: BuildState, size: &str) -> Markup {
    let label = state_label(state);
    html! { span class={"status " (kind(state)) " " (size)} role="img" aria-label=(label) title=(label) { (icon(state_icon(state))) } }
}

fn task_status(state: TaskState, size: &str) -> Markup {
    let (class, glyph) = task_kind(state);
    let label = task_label(state);
    html! { span class={"status " (class) " " (size)} role="img" aria-label=(label) title=(label) { (icon(glyph)) } }
}

pub fn time(seconds: i64) -> Markup {
    html! { time data-time=(seconds) { "" } }
}

fn elapsed(started: Option<i64>, finished: Option<i64>) -> Markup {
    match (started, finished) {
        (Some(start), Some(end)) => html! { (duration(end - start)) },
        (Some(start), None) => html! { span data-started=(start) { "" } },
        _ => html! {},
    }
}

fn released(build: &Build) -> bool {
    build.state == BuildState::Succeeded && build.detail.as_deref().is_some_and(|d| d.starts_with("Released"))
}

pub fn trigger_pill(build: &Build) -> Markup {
    match build.trigger {
        Trigger::Push => html! { span class="pill" { (icon(Icon::Branch)) (build.ref_name) } },
        Trigger::Tag => html! { span class="pill accent" { (icon(Icon::Tag)) (build.ref_name) } },
        Trigger::PullRequest => {
            html! { span class="pill" { (icon(Icon::Pull)) "#" (build.pr_number.unwrap_or_default()) } }
        }
        Trigger::Manual => html! { span class="pill" title="Started by hand" { (icon(Icon::Play)) (build.ref_name) } },
    }
}

fn version_pill(build: &Build) -> Markup {
    match &build.version {
        Some(version) if released(build) => {
            html! { span class="pill mono ok" title="Released" { (icon(Icon::Tag)) (version) } }
        }
        Some(version) => html! { span class="pill mono" title="Version, not released" { (version) } },
        None => html! {},
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

fn initial(name: &str) -> String {
    name.chars().next().map(|c| c.to_uppercase().collect()).unwrap_or_default()
}

pub fn build_row(build: &Build, repo: Option<&Repo>, show_repo: bool) -> Markup {
    let href = repo.map(|r| build_path(r, build)).unwrap_or_default();
    let live = build.state.is_active() || build.state == BuildState::Queued;
    let failed = kind(build.state) == "bad";
    html! {
        li class=(if failed { "build-row failed" } else { "build-row" }) {
            (status(build.state, ""))
            div class="main" {
                a class="title" href=(href) title=(commit_title(build)) { (commit_title(build)) }
                div class="sub" {
                    @if show_repo { @if let Some(repo) = repo { a href=(repo_path(repo)) { (repo.name) } } }
                    span { "#" (build.number) }
                    @if live || failed {
                        span class="sep" { "·" }
                        span class={"detail " (kind(build.state))} { (headline(build)) }
                    } @else if let Some(actor) = &build.actor {
                        span class="sep" { "·" }
                        span { (actor) }
                    }
                }
            }
            div class="ref" { (trigger_pill(build)) span class="sha" { (build.short_sha()) } }
            div class="version" { (version_pill(build)) }
            div class="when" {
                span class="duration" { (elapsed(build.started_at, build.finished_at)) }
                (time(build.created_at))
            }
        }
    }
}

pub fn layout(title: &str, nav: &str, ctx: &Context<'_>, attrs: &[(&str, String)], content: Markup) -> Markup {
    let data: String = attrs.iter().map(|(k, v)| format!(" data-{k}=\"{}\"", html_escape(v))).collect();
    let item = |id: &str, href: &str, glyph: Icon, label: &str| {
        html! { a class=(if nav == id { "active" } else { "" }) href=(href) title=(label) aria-current=[(nav == id).then_some("page")] { (icon(glyph)) span class="label" { (label) } } }
    };
    html! {
        (DOCTYPE)
        html lang="en" {
            (head(title))
            (PreEscaped(format!("<body{data}>")))
            header class="topbar" {
                div class="inner" {
                    a class="brand" href="/" { span class="mark" { (PreEscaped(LOGO)) } span class="word" { "Kiln" } }
                    nav class="nav" aria-label="Main" {
                        (item("builds", "/", Icon::Activity, "Builds"))
                        (item("repos", "/repos", Icon::Book, "Repositories"))
                        (item("system", "/system", Icon::Server, "System"))
                        (item("settings", "/settings", Icon::Cog, "Settings"))
                    }
                    div class="right" {
                        span class="live" title="Live updates" {
                            span class="dot" {}
                            span class="state-text connecting" { "Connecting" }
                            span class="state-text connected" { "Live" }
                            span class="state-text reconnecting" { "Reconnecting" }
                        }
                        details class="menu" {
                            summary class="avatar" aria-label="Account" {
                                @if ctx.viewer.avatar_url.is_empty() { (initial(&ctx.viewer.login)) } @else { img src=(ctx.viewer.avatar_url) alt=""; }
                            }
                            div class="menu-panel" {
                                div class="label" { "Signed in as" strong { (ctx.viewer.login) } }
                                @if ctx.demo { div class="label" { span class="pill warn" { "Demo mode" } } }
                                hr;
                                a class="menu-item" href="/system" { (icon(Icon::Server)) "System" }
                                form method="post" action="/logout" { button class="menu-item" type="submit" { (icon(Icon::LogOut)) "Sign out" } }
                            }
                        }
                    }
                }
            }
            main {
                @if let Some(note) = ctx.registry_note.as_ref().filter(|_| nav != "settings") {
                    div class="banner warn" {
                        (icon(Icon::Key))
                        div class="text" { strong { (note) } }
                        a class="button small" href="/settings#registry" { "Add token" }
                    }
                }
                (content)
            }
            div class="toasts" aria-live="polite" {}
            (PreEscaped("</body>"))
        }
    }
}

fn head(title: &str) -> Markup {
    html! {
        head {
            meta charset="utf-8";
            meta name="viewport" content="width=device-width, initial-scale=1";
            meta name="color-scheme" content="light dark";
            title { (title) " · Kiln" }
            link rel="icon" href="/assets/favicon.svg" type="image/svg+xml";
            link rel="stylesheet" href={"/assets/kiln.css?v=" (*ASSET_VERSION)};
            script src={"/assets/kiln.js?v=" (*ASSET_VERSION)} defer {}
        }
    }
}

fn html_escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('"', "&quot;").replace('<', "&lt;").replace('>', "&gt;")
}

fn empty(glyph: Icon, title: &str, body: Markup) -> Markup {
    html! { div class="empty" { span class="glyph" { (icon(glyph)) } h3 { (title) } (body) } }
}

fn refresh_button() -> Markup {
    html! {
        form method="post" action="/system/refresh" {
            button class="button" type="submit" title="Check for new commits" { (icon(Icon::Refresh)) "Check GitHub" }
        }
    }
}

pub fn login(error: Option<&str>, demo: bool) -> Markup {
    html! {
        (DOCTYPE)
        html lang="en" {
            (head("Sign in"))
            body {
                div class="auth" {
                    div class="card auth-card" {
                        span class="mark" { (PreEscaped(LOGO)) }
                        h1 { "Sign in to Kiln" }
                        p { "Self-hosted CI" }
                        @if let Some(error) = error {
                            div class="banner bad" { (icon(Icon::Alert)) div class="text" { (error) } }
                        }
                        @if demo {
                            a class="button primary" href="/auth/github" { (icon(Icon::Play)) "Enter the demo" }
                        } @else {
                            a class="button primary" href="/auth/github" { (PreEscaped(GITHUB)) "Continue with GitHub" }
                        }
                    }
                }
            }
        }
    }
}

fn auth_page(title: &str, scripts: bool, body: Markup) -> Markup {
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                meta name="color-scheme" content="light dark";
                title { (title) " · Kiln" }
                link rel="icon" href="/assets/favicon.svg" type="image/svg+xml";
                link rel="stylesheet" href={"/assets/kiln.css?v=" (*ASSET_VERSION)};
                @if scripts { script src={"/assets/connect.js?v=" (*ASSET_VERSION)} defer {} }
            }
            body { div class="auth" { div class="card auth-card" { span class="mark" { (PreEscaped(LOGO)) } (body) } } }
        }
    }
}

pub fn connect(error: Option<&str>) -> Markup {
    auth_page(
        "Connect GitHub",
        false,
        html! {
            h1 { "Connect GitHub" }
            p { "Kiln creates its GitHub App for you." }
            @if let Some(error) = error {
                div class="banner bad" { (icon(Icon::Alert)) div class="text" { (error) } }
            }
            form class="connect" method="post" action="/setup" {
                label class="field" {
                    span { "Setup code" }
                    input class="input" name="code" required autocomplete="off" autocapitalize="characters" spellcheck="false" placeholder="XXXX-XXXX-XXXX";
                    small { "In Kiln's log: " code { "kubectl -n kiln logs deploy/kiln" } }
                }
                label class="field" {
                    span { "Organization " em { "optional" } }
                    input class="input" name="org" autocomplete="off" spellcheck="false" placeholder="Your personal account";
                }
                button class="button primary" type="submit" { (PreEscaped(GITHUB)) "Connect GitHub" }
            }
        },
    )
}

pub fn claim(error: Option<&str>) -> Markup {
    auth_page(
        "Claim Kiln",
        false,
        html! {
            h1 { "Claim Kiln" }
            p { "Nobody can sign in yet. Enter the setup code to become the first." }
            @if let Some(error) = error {
                div class="banner bad" { (icon(Icon::Alert)) div class="text" { (error) } }
            }
            form class="connect" method="post" action="/claim" {
                label class="field" {
                    span { "Setup code" }
                    input class="input" name="code" required autocomplete="off" autocapitalize="characters" spellcheck="false" placeholder="XXXX-XXXX-XXXX";
                    small { "In Kiln's log: " code { "kubectl -n kiln logs deploy/kiln" } }
                }
                button class="button primary" type="submit" { "Claim" }
            }
        },
    )
}

pub fn connect_redirect(action: &str, manifest: &str) -> Markup {
    auth_page(
        "Connect GitHub",
        true,
        html! {
            h1 { "Opening GitHub…" }
            form method="post" action=(action) data-autosubmit {
                input type="hidden" name="manifest" value=(manifest);
                button class="button primary" type="submit" { (PreEscaped(GITHUB)) "Continue on GitHub" }
            }
        },
    )
}

pub fn connected(name: &str, install: &str) -> Markup {
    auth_page(
        "Connected",
        false,
        html! {
            h1 { "Connected" }
            p { (name) " is ready. Pick the repositories Kiln builds, then sign in." }
            div class="stack-buttons" {
                a class="button primary" href=(install) target="_blank" rel="noopener" { (PreEscaped(GITHUB)) "Choose repositories" }
                a class="button" href="/login" { "Sign in" }
            }
        },
    )
}

pub fn error_page(ctx: Option<&Context<'_>>, status: u16, message: &str) -> Markup {
    let title = match status {
        404 => "Not found",
        403 => "Not allowed",
        400 => "Something is off",
        _ => "Kiln hit an error",
    };
    let content = html! {
        div class="card" {
            (empty(if status == 404 { Icon::Search } else { Icon::Alert }, title, html! {
                p { (message) }
                a class="button" href="/" { "Back to builds" }
            }))
        }
    };
    match ctx {
        Some(ctx) => layout(title, "", ctx, &[], content),
        None => html! { (DOCTYPE) html lang="en" { (head(title)) body { main { (content) } } } },
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Status {
    #[default]
    All,
    Passed,
    Failed,
    Stopped,
}

impl Status {
    pub fn parse(value: Option<&str>) -> Self {
        match value {
            Some("passed") => Status::Passed,
            Some("failed") => Status::Failed,
            Some("stopped") => Status::Stopped,
            _ => Status::All,
        }
    }

    pub fn states(self) -> Vec<BuildState> {
        match self {
            Status::All => vec![
                BuildState::Succeeded,
                BuildState::Failed,
                BuildState::Errored,
                BuildState::Cancelled,
                BuildState::Superseded,
                BuildState::Skipped,
            ],
            Status::Passed => vec![BuildState::Succeeded],
            Status::Failed => vec![BuildState::Failed, BuildState::Errored],
            Status::Stopped => vec![BuildState::Cancelled, BuildState::Superseded, BuildState::Skipped],
        }
    }

    fn key(self) -> Option<&'static str> {
        match self {
            Status::All => None,
            Status::Passed => Some("passed"),
            Status::Failed => Some("failed"),
            Status::Stopped => Some("stopped"),
        }
    }
}

#[derive(Default)]
pub struct Stats {
    pub passed_day: usize,
    pub failed_day: usize,
    pub hours: Vec<(usize, usize)>,
    pub recent: Vec<BuildState>,
    pub rate: Option<(usize, usize)>,
    pub median: Option<i64>,
}

pub struct DashboardData {
    pub repos: Vec<Repo>,
    pub active: Vec<Build>,
    pub queued: Vec<Build>,
    pub builds: Vec<Build>,
    pub more: Option<i64>,
    pub status: Status,
    pub repo: Option<i64>,
    pub slots: usize,
    pub stats: Stats,
}

fn dashboard_query(status: Status, repo: Option<i64>, before: Option<i64>) -> String {
    let parts: Vec<String> = [
        status.key().map(|s| format!("status={s}")),
        repo.map(|r| format!("repo={r}")),
        before.map(|b| format!("before={b}")),
    ]
    .into_iter()
    .flatten()
    .collect();
    if parts.is_empty() { "/".into() } else { format!("/?{}", parts.join("&")) }
}

pub fn dashboard(ctx: &Context<'_>, data: &DashboardData) -> Markup {
    layout(
        "Builds",
        "builds",
        ctx,
        &[("page", "dashboard".into())],
        html! {
            div class="page-head" {
                div class="titles" { h1 { "Builds" } }
                div class="actions" { (refresh_button()) }
            }
            div id="live" { (dashboard_live(data)) }
        },
    )
}

pub fn dashboard_live(data: &DashboardData) -> Markup {
    let repo = |id: i64| data.repos.iter().find(|r| r.id == id);
    let enrolled: Vec<&Repo> = data.repos.iter().filter(|r| r.enrolled && r.removed_at.is_none()).collect();
    let stats = &data.stats;
    let day_total = stats.passed_day + stats.failed_day;
    html! {
        div class="tiles" {
            div class="card tile" {
                span class="label" { (icon(Icon::Gauge)) "Build slots" }
                span class="value" { (data.active.len()) small { "of " (data.slots) " busy" } }
                div class="meter" { @for slot in 0..data.slots { span class=(if slot < data.active.len() { "on" } else { "" }) {} } }
            }
            div class="card tile" {
                span class="label" { (icon(Icon::Clock)) "Queued" }
                span class="value" { (data.queued.len()) }
                span class="foot" {
                    @match data.queued.first() {
                        Some(next) => { "Next: " @if let Some(r) = repo(next.repo_id) { (r.name) } " #" (next.number) },
                        None => "Nothing is waiting",
                    }
                }
            }
            div class="card tile" {
                span class="label" { (icon(Icon::Activity)) "Last 24 hours" }
                span class="value" { (day_total) small { @if day_total == 1 { "build" } @else { "builds" } } }
                span class="foot" { (stats.passed_day) " passed · " (stats.failed_day) " failed" }
                @let peak = stats.hours.iter().map(|(ok, bad)| ok + bad).max().unwrap_or(0).max(1);
                div class="spark" aria-hidden="true" {
                    @for (ok, bad) in &stats.hours {
                        span { i class="bad" style={"height:" ((bad * 100) / peak) "%"} {} i class="ok" style={"height:" ((ok * 100) / peak) "%"} {} }
                    }
                }
            }
            div class="card tile" {
                span class="label" { (icon(Icon::Check)) "Success rate" }
                @match stats.rate {
                    Some((passed, total)) => {
                        span class={"value " (if passed * 10 >= total * 9 { "ok" } else if passed * 2 < total { "bad" } else { "" })} { (passed * 100 / total.max(1)) "%" }
                        span class="foot" { "Last " (total) @if let Some(median) = stats.median { " · median " (duration(median)) } }
                        div class="results" aria-hidden="true" { @for state in &stats.recent { span class=(kind(*state)) {} } }
                    },
                    None => { span class="value" { "–" } span class="foot" { "No finished builds yet" } },
                }
            }
        }
        div class="stack" {
            @if !data.active.is_empty() || !data.queued.is_empty() {
                section class="card" {
                    div class="card-head" { h2 { "In progress" span class="count" { (data.active.len() + data.queued.len()) } } }
                    ul class="builds" {
                        @for build in data.active.iter().chain(&data.queued) { (build_row(build, repo(build.repo_id), true)) }
                    }
                }
            }
            section class="card" {
                div class="card-head" {
                    h2 { "History" }
                    div class="tools" {
                        nav class="segmented" aria-label="Filter by result" {
                            @for (status, label) in [(Status::All, "All"), (Status::Passed, "Passed"), (Status::Failed, "Failed"), (Status::Stopped, "Cancelled")] {
                                a class=(if status == data.status { "active" } else { "" }) href=(dashboard_query(status, data.repo, None)) { (label) }
                            }
                        }
                        @if enrolled.len() > 1 {
                            form class="filters" method="get" action="/" {
                                @if let Some(key) = data.status.key() { input type="hidden" name="status" value=(key); }
                                select class="select" name="repo" aria-label="Repository" data-autosubmit {
                                    option value="" { "All repositories" }
                                    @for r in &enrolled { option value=(r.id) selected[data.repo == Some(r.id)] { (r.name) } }
                                }
                                noscript { button class="button small" type="submit" { "Apply" } }
                            }
                        }
                    }
                }
                @if data.builds.is_empty() {
                    @if enrolled.is_empty() {
                        (onboarding())
                    } @else if data.status != Status::All || data.repo.is_some() {
                        (empty(Icon::Search, "No matching builds", html! { a class="button small" href="/" { "Clear filters" } }))
                    } @else {
                        (empty(Icon::Activity, "No builds yet", html! {}))
                    }
                } @else {
                    ul class="builds" { @for build in &data.builds { (build_row(build, repo(build.repo_id), true)) } }
                    @if let Some(before) = data.more {
                        div class="card-foot" { a class="button small" href=(dashboard_query(data.status, data.repo, Some(before))) { "Older builds" } }
                    }
                }
            }
        }
    }
}

fn onboarding() -> Markup {
    empty(
        Icon::Rocket,
        "Add Kiln to a repository",
        html! {
            p { "Commit a " code { (FILE_NAME) } " to a repository's default branch." }
            pre { "version = 1\n\n[test.unit]\nimage = \"node:26\"\nsteps = [\"npm ci\", \"npm test\"]" }
        },
    )
}

pub struct RepoRow {
    pub repo: Repo,
    pub last: Option<Build>,
    pub version: Option<String>,
    pub recent: Vec<Build>,
}

pub fn repos(ctx: &Context<'_>, rows: &[RepoRow]) -> Markup {
    layout(
        "Repositories",
        "repos",
        ctx,
        &[("page", "repos".into())],
        html! {
            div class="page-head" {
                div class="titles" { h1 { "Repositories" } }
                div class="actions" {
                    label class="search" {
                        (icon(Icon::Search))
                        span class="sr-only" { "Filter repositories" }
                        input class="input" type="search" placeholder="Filter repositories" data-filter-rows autocomplete="off";
                    }
                    (refresh_button())
                }
            }
            div id="live" { (repos_live(rows)) }
        },
    )
}

pub fn repos_live(rows: &[RepoRow]) -> Markup {
    let (used, other): (Vec<&RepoRow>, Vec<&RepoRow>) =
        rows.iter().filter(|r| r.repo.removed_at.is_none()).partition(|r| r.repo.enrolled);
    let gone: Vec<&RepoRow> = rows.iter().filter(|r| r.repo.removed_at.is_some()).collect();
    html! {
        div class="stack" {
            section class="card" {
                div class="card-head" { h2 { "Using Kiln" span class="count" { (used.len()) } } }
                @if used.is_empty() {
                    (onboarding())
                } @else {
                    div class="table-wrap" {
                        table class="table" {
                            thead { tr { th { "Repository" } th { "Latest build" } th class="wide-only" { "Recent" } th class="wide-only" { "Released" } } }
                            tbody {
                                @for row in &used {
                                    @let repo = &row.repo;
                                    tr class="row-link" data-name=(repo.full_name()) {
                                        td {
                                            div class="cell" {
                                                span class="repo-glyph" { (initial(&repo.name)) }
                                                div class="stacked" {
                                                    a class="cover" href=(repo_path(repo)) { strong { (repo.name) } }
                                                    span { (repo.owner) }
                                                }
                                                @if repo.paused { span class="pill warn" { (icon(Icon::Pause)) "Paused" } }
                                                @if repo.config_error.is_some() { span class="pill bad" { "Config error" } }
                                                @if repo.drift.is_some() { span class="pill warn" { (icon(Icon::Alert)) "Registry" } }
                                            }
                                        }
                                        td {
                                            @match &row.last {
                                                Some(build) => div class="cell" {
                                                    (status(build.state, "sm"))
                                                    div class="stacked" {
                                                        strong { (commit_title(build)) }
                                                        span { "#" (build.number) " · " (time(build.created_at)) }
                                                    }
                                                },
                                                None => span class="muted" { "No builds yet" },
                                            }
                                        }
                                        td class="wide-only" { (recent_strip(repo, &row.recent)) }
                                        td class="wide-only" {
                                            @match &row.version { Some(version) => span class="pill mono ok" { (version) }, None => span class="muted" { "–" } }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            @if !other.is_empty() {
                section class="card" {
                    div class="card-head" { h2 { "Without " code { (FILE_NAME) } span class="count" { (other.len()) } } }
                    div class="table-wrap" {
                        table class="table" {
                            tbody {
                                @for row in &other {
                                    @let repo = &row.repo;
                                    tr class="row-link dim" data-name=(repo.full_name()) {
                                        td {
                                            div class="cell" {
                                                span class="repo-glyph" { (initial(&repo.name)) }
                                                div class="stacked" { a class="cover" href=(repo_path(repo)) { strong { (repo.name) } } span { (repo.owner) } }
                                            }
                                        }
                                        td class="when right" { span class="pill" { (icon(Icon::Branch)) (repo.default_branch) } }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            @if !gone.is_empty() {
                section class="card" {
                    div class="card-head" { h2 { "No longer visible" span class="count" { (gone.len()) } } }
                    div class="table-wrap" {
                        table class="table" {
                            tbody {
                                @for row in &gone {
                                    tr class="row-link dim" data-name=(row.repo.full_name()) {
                                        td { div class="cell" { span class="repo-glyph" { (initial(&row.repo.name)) } div class="stacked" { a class="cover" href=(repo_path(&row.repo)) { strong { (row.repo.name) } } span { (row.repo.owner) } } } }
                                        td class="when right" { @if let Some(at) = row.repo.removed_at { "Removed " (time(at)) } }
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

fn recent_strip(repo: &Repo, builds: &[Build]) -> Markup {
    html! {
        span class="recent" {
            @for build in builds {
                a class=(kind(build.state)) href=(build_path(repo, build)) title={"#" (build.number) " · " (state_label(build.state))} aria-label={"Build " (build.number) " " (state_label(build.state))} {}
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
    pub history: Vec<Build>,
    pub releases: Vec<(String, i64, i64)>,
    pub config: Option<Config>,
    pub more: Option<i64>,
    pub busy: bool,
    pub secret: String,
}

pub fn repo(ctx: &Context<'_>, data: &RepoData) -> Markup {
    let repo = &data.repo;
    layout(
        &repo.name,
        "repos",
        ctx,
        &[("page", "repo".into()), ("repo", repo.id.to_string())],
        html! { div id="live" { (repo_live(ctx, data)) } },
    )
}

fn rate(builds: &[Build]) -> Option<(usize, usize)> {
    let judged: Vec<&Build> = builds
        .iter()
        .filter(|b| matches!(b.state, BuildState::Succeeded | BuildState::Failed | BuildState::Errored))
        .collect();
    (!judged.is_empty()).then(|| (judged.iter().filter(|b| b.state == BuildState::Succeeded).count(), judged.len()))
}

pub fn median(builds: &[Build]) -> Option<i64> {
    let mut times: Vec<i64> = builds
        .iter()
        .filter(|b| b.state == BuildState::Succeeded)
        .filter_map(|b| Some(b.finished_at? - b.started_at?))
        .collect();
    times.sort_unstable();
    times.get(times.len() / 2).copied()
}

pub fn repo_live(ctx: &Context<'_>, data: &RepoData) -> Markup {
    let repo = &data.repo;
    let github = format!("{}/{}", ctx.github_web, repo.full_name());
    let last = data.history.last();
    html! {
        nav class="crumbs" aria-label="Breadcrumb" { a href="/repos" { "Repositories" } span class="sep" { "/" } span { (repo.owner) } }
        div class="page-head" {
            div class="titles" {
                div class="title-row" {
                    span class="repo-glyph" { (initial(&repo.name)) }
                    div {
                        h1 { (repo.name) }
                        div class="chips" {
                            span class="pill" { (icon(Icon::Branch)) (repo.default_branch) }
                            @if let Some(head) = &repo.head_sha { a class="mono" href={(github) "/commit/" (head)} target="_blank" rel="noopener" { (icon(Icon::Commit)) (&head[..head.len().min(7)]) } }
                            a href=(github) target="_blank" rel="noopener" { (PreEscaped(GITHUB)) "GitHub" (icon(Icon::External)) }
                        }
                    }
                }
            }
            div class="actions" {
                @if repo.enrolled && !repo.paused && repo.removed_at.is_none() {
                    form method="post" action={(repo_path(repo)) "/build"} {
                        button class="button primary" type="submit" { (icon(Icon::Play)) "Build " (repo.default_branch) }
                    }
                }
                @if repo.paused {
                    form method="post" action={(repo_path(repo)) "/resume"} { button class="button" type="submit" { (icon(Icon::Play)) "Resume" } }
                }
                @if repo.enrolled {
                    details class="menu" {
                        summary class="button icon-only" aria-label="More actions" title="More actions" { (icon(Icon::More)) }
                        div class="menu-panel" {
                            @if !repo.paused {
                                form method="post" action={(repo_path(repo)) "/pause"} data-confirm="New pushes are ignored until you resume." data-confirm-title={"Pause " (repo.name) "?"} data-confirm-action="Pause" {
                                    button class="menu-item" type="submit" { (icon(Icon::Pause)) "Pause builds" }
                                }
                            }
                            form method="post" action={(repo_path(repo)) "/reset-cache"} data-confirm="The next build starts cold." data-confirm-title="Reset the build cache?" data-confirm-action="Reset cache" data-confirm-danger {
                                button class="menu-item danger" type="submit" disabled[data.busy] title=(if data.busy { "Wait until the running build finishes" } else { "" }) { (icon(Icon::Trash)) "Reset build cache" }
                            }
                        }
                    }
                }
            }
        }
        @if repo.paused {
            div class="banner warn" { (icon(Icon::Pause)) div class="text" { strong { "Builds paused" } } }
        }
        @if let Some(drift) = &repo.drift {
            div class="banner warn" {
                (icon(Icon::Alert))
                div class="text" {
                    strong { "Published outside Kiln" }
                    span class="hint" { code { (drift) } }
                    form method="post" action={(repo_path(repo)) "/dismiss-drift"} { button class="button small" type="submit" { "Dismiss" } }
                }
            }
        }
        @if let Some(error) = &repo.config_error {
            div class="banner bad" { (icon(Icon::Alert)) div class="text" { strong { (FILE_NAME) " on " (repo.default_branch) " has errors" } pre { (error) } } }
        }
        @if !repo.enrolled && repo.removed_at.is_none() {
            div class="banner wait" { (icon(Icon::Clock)) div class="text" { strong { "No " code { (FILE_NAME) } " on " (repo.default_branch) } } }
        }
        @if repo.enrolled || !data.history.is_empty() {
            div class="tiles" {
                @match data.releases.first() {
                    Some((version, number, at)) => a class="card tile" href={(repo_path(repo)) "/builds/" (number)} {
                        span class="label" { (icon(Icon::Tag)) "Latest release" }
                        span class="value mono" { (version) }
                        span class="foot" { (time(*at)) " · #" (number) }
                    },
                    None => div class="card tile" {
                        span class="label" { (icon(Icon::Tag)) "Latest release" }
                        span class="value" { "–" }
                        span class="foot" { "Nothing released yet" }
                    },
                }
                div class="card tile" {
                    span class="label" { (icon(Icon::Check)) "Success rate" }
                    @match rate(&data.history) {
                        Some((passed, total)) => { span class="value" { (passed * 100 / total) "%" } span class="foot" { "Last " (total) " finished builds" } },
                        None => { span class="value" { "–" } span class="foot" { "No finished builds" } },
                    }
                }
                div class="card tile" {
                    span class="label" { (icon(Icon::Timer)) "Median duration" }
                    @match median(&data.history) {
                        Some(seconds) => { span class="value" { (duration(seconds)) } span class="foot" { "Of passing builds" } },
                        None => { span class="value" { "–" } span class="foot" { "No passing builds" } },
                    }
                }
                @match last {
                    Some(build) => a class="card tile" href=(build_path(repo, build)) {
                        span class="label" { (icon(Icon::Activity)) "Latest build" }
                        span class={"value " (kind(build.state))} { (state_label(build.state)) }
                        span class="foot" { "#" (build.number) " · " (time(build.created_at)) }
                    },
                    None => div class="card tile" {
                        span class="label" { (icon(Icon::Activity)) "Latest build" }
                        span class="value" { "–" }
                        span class="foot" { "No builds yet" }
                    },
                }
            }
        }
        div class="columns" {
            div class="stack" {
                @if data.history.len() > 1 {
                    section class="card" {
                        div class="card-head" { h2 { "Build durations" span class="count" { "last " (data.history.len()) } } }
                        (history_chart(repo, &data.history))
                    }
                }
                section class="card" {
                    div class="card-head" {
                        nav class="segmented" aria-label="Filter builds" {
                            @for (show, label) in [(Show::All, "All"), (Show::Branch, repo.default_branch.as_str()), (Show::Pulls, "Pull requests"), (Show::Tags, "Tags")] {
                                a class=(if show == data.show { "active" } else { "" }) href={(repo_path(repo)) (show.query())} { (label) }
                            }
                        }
                    }
                    @if data.builds.is_empty() {
                        (empty(Icon::Activity, match data.show { Show::All => "No builds yet", Show::Branch => "No branch builds yet", Show::Pulls => "No pull request builds yet", Show::Tags => "No tag builds yet" }, html! {}))
                    } @else {
                        ul class="builds" { @for build in &data.builds { (build_row(build, Some(repo), false)) } }
                        @if let Some(before) = data.more {
                            @let separator = if data.show == Show::All { "?" } else { "&" };
                            div class="card-foot" { a class="button small" href={(repo_path(repo)) (data.show.query()) (separator) "before=" (before)} { "Older builds" } }
                        }
                    }
                }
            }
            aside class="stack" {
                section class="card" {
                    div class="card-head" { h2 { "Releases" } }
                    @if data.releases.is_empty() {
                        div class="empty compact" { p { "Nothing released yet" } }
                    } @else {
                        ul class="releases" {
                            @for (version, number, at) in &data.releases {
                                li {
                                    span class="pill mono ok" { (icon(Icon::Tag)) (version) }
                                    a class="grow cover" href={(repo_path(repo)) "/builds/" (number)} { "#" (number) }
                                    span class="when" { (time(*at)) }
                                }
                            }
                        }
                    }
                }
                @if let Some(config) = &data.config {
                    section class="card" {
                        div class="card-head" { h2 { "Pipeline" } span class="pill mono" { (FILE_NAME) } }
                        dl class="facts" {
                            dt { "Runs on" } dd { (triggers(&config.on)) }
                            dt { "Tests" } dd { @if config.test.is_empty() { span class="muted" { "none" } } @for name in config.test.keys() { span class="pill" { (name) } } }
                            dt { "Images" } dd { @for image in &config.images { span class="pill" { (icon(Icon::Box)) (image.name) } } }
                            dt { "Verify" } dd { @if config.verify.is_empty() { span class="muted" { "none" } } @for name in config.verify.keys() { span class="pill" { (name) } } }
                            dt { "Releases" } dd {
                                (match config.release.mode { ReleaseMode::Rc => "Every default-branch push and tag", ReleaseMode::Tags => "Tags only" })
                                @if config.release.shadow { span class="pill warn" { "Shadow" } }
                            }
                            dt { "Secret" } dd { code title="Repository secrets" { (data.secret) } }
                        }
                    }
                }
            }
        }
    }
}

fn history_chart(repo: &Repo, builds: &[Build]) -> Markup {
    let seconds = |b: &Build| match (b.started_at, b.finished_at) {
        (Some(start), Some(end)) => end - start,
        _ => 0,
    };
    let max = builds.iter().map(seconds).max().unwrap_or(0).max(1);
    html! {
        div class="history" role="img" aria-label="Duration of recent builds" {
            @for build in builds {
                @let height = (seconds(build) * 100 / max).max(8);
                a class=(kind(build.state)) href=(build_path(repo, build)) style={"height:" (height) "%"}
                    title={"#" (build.number) " · " (state_label(build.state)) @if seconds(build) > 0 { " · " (duration(seconds(build))) }} {}
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
                div id="tasks" class="side-col" { (sidebar(ctx, data, selected)) }
                div class="stack" {
                    div id="log" {
                        div class="log" {
                            div class="log-empty" { @if data.tasks.is_empty() { (no_tasks_message(&data.build)) } @else { "Loading log…" } }
                            @if !data.tasks.is_empty() {
                                noscript {
                                    div class="log-empty" {
                                        @for task in &data.tasks { a href={"/builds/" (data.build.id) "/logs/" (data.attempt) "/" (task.ordinal) ".txt"} { (task_title(task)) } " " }
                                    }
                                }
                            }
                        }
                    }
                    div id="images" { @if data.images.iter().any(|i| i.digest.is_some()) { (images_card(&data.images)) } }
                }
            }
        },
    )
}

fn no_tasks_message(build: &Build) -> &'static str {
    match build.state {
        BuildState::Queued if build.detail.as_deref() == Some(crate::server::engine::WAITING_FOR_TOKEN) => {
            crate::server::engine::WAITING_FOR_TOKEN
        }
        BuildState::Queued => "Waiting for a build slot",
        BuildState::Skipped => "Skipped",
        BuildState::Failed if build.failure == Some(Failure::Config) => "Invalid configuration",
        _ if build.trigger == Trigger::Tag => "Released the verified image",
        _ => "Nothing ran",
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
        span hidden data-build-state=(kind(build.state)) data-build-label=(state_label(build.state)) {}
        nav class="crumbs" aria-label="Breadcrumb" {
            a href="/repos" { "Repositories" }
            span class="sep" { "/" }
            a href=(repo_path(repo)) { (repo.name) }
            span class="sep" { "/" }
            span { "Build #" (build.number) }
        }
        div class="page-head" {
            div class="titles" {
                div class="title-row" {
                    (status(build.state, "lg"))
                    div {
                        h1 { (commit_title(build)) }
                        div class="chips" {
                            a href=(trigger_url(ctx, repo, build)) target="_blank" rel="noopener" { (trigger_pill(build)) }
                            a class="mono" href=(commit_url) target="_blank" rel="noopener" title="Open the commit on GitHub" { (icon(Icon::Commit)) (build.short_sha()) }
                            (version_pill(build))
                            @if let Some(actor) = &build.actor { span { (icon(Icon::User)) (actor) } }
                            span { (icon(Icon::Clock)) (time(build.created_at)) }
                            @if build.started_at.is_some() { span { (icon(Icon::Timer)) (elapsed(build.started_at, build.finished_at)) } }
                        }
                    }
                }
            }
            div class="actions" {
                @if matches!(build.state, BuildState::Queued | BuildState::Starting | BuildState::Running)
                    || (build.state == BuildState::Releasing && data.images.iter().all(|i| i.released.is_none())) {
                    form method="post" action={(path) "/cancel"} data-confirm="Nothing is released." data-confirm-title={"Cancel build #" (build.number) "?"} data-confirm-action="Cancel build" data-confirm-danger {
                        button class="button danger" type="submit" { (icon(Icon::Stop)) "Cancel" }
                    }
                }
                @if build.state.is_finished() && !released_tag {
                    form method="post" action={(path) "/rerun"} {
                        button class=(if kind(build.state) == "bad" { "button primary" } else { "button" }) type="submit" { (icon(Icon::Rerun)) "Rerun" }
                    }
                }
            }
        }
        (pipeline(data))
        (banner(build, data.ahead))
    }
}

struct Stage {
    name: &'static str,
    class: &'static str,
    state: TaskState,
    info: Markup,
    first: Option<i64>,
}

fn stages(data: &BuildData) -> Vec<Stage> {
    let build = &data.build;
    let mut out = Vec::new();
    for (name, kinds) in [
        ("Source", &[TaskKind::Fetch][..]),
        ("Test", &[TaskKind::Test][..]),
        ("Build", &[TaskKind::Image, TaskKind::Push][..]),
        ("Verify", &[TaskKind::Verify][..]),
    ] {
        let tasks: Vec<&Task> = data.tasks.iter().filter(|t| kinds.contains(&t.kind)).collect();
        if tasks.is_empty() {
            continue;
        }
        let any = |s: &[TaskState]| tasks.iter().any(|t| s.contains(&t.state));
        let all = |s: &[TaskState]| tasks.iter().all(|t| s.contains(&t.state));
        let state = if any(&[TaskState::Failed, TaskState::TimedOut]) {
            TaskState::Failed
        } else if any(&[TaskState::Running]) || (any(&[TaskState::Passed]) && any(&[TaskState::Pending])) {
            TaskState::Running
        } else if all(&[TaskState::Passed]) {
            TaskState::Passed
        } else if all(&[TaskState::Skipped, TaskState::Cancelled, TaskState::Passed]) && !all(&[TaskState::Passed]) {
            if any(&[TaskState::Cancelled]) { TaskState::Cancelled } else { TaskState::Skipped }
        } else {
            TaskState::Pending
        };
        let start = tasks.iter().filter_map(|t| t.started_at).min();
        let end = tasks.iter().filter_map(|t| t.finished_at).max();
        let count = match name {
            "Test" | "Verify" => Some(format!("{} {}", tasks.len(), if tasks.len() == 1 { "job" } else { "jobs" })),
            "Build" => {
                let images = tasks.iter().filter(|t| t.kind == TaskKind::Image).count();
                Some(format!("{images} {}", if images == 1 { "image" } else { "images" }))
            }
            _ => None,
        };
        let info = html! {
            @if let Some(count) = &count { (count) }
            @if count.is_some() { " · " }
            @match state {
                TaskState::Running => (elapsed(start, None)),
                TaskState::Pending | TaskState::Skipped | TaskState::Cancelled if start.is_none() => (task_label(state)),
                _ => (elapsed(start, end)),
            }
        };
        let first = tasks
            .iter()
            .find(|t| matches!(t.state, TaskState::Failed | TaskState::TimedOut | TaskState::Running))
            .or(tasks.first())
            .map(|t| t.ordinal);
        out.push(Stage { name, class: task_kind(state).0, state, info, first });
    }
    if build.trigger != Trigger::PullRequest && (build.version.is_some() || !data.tasks.is_empty()) {
        let version = build.version.clone().unwrap_or_default();
        let (state, info) = if released(build) {
            (TaskState::Passed, html! { (version) })
        } else if build.state == BuildState::Releasing {
            (TaskState::Running, html! { "Publishing " (version) })
        } else if build.failure == Some(Failure::Release) || build.failure == Some(Failure::Version) {
            (TaskState::Failed, html! { "Failed" })
        } else if build.state == BuildState::Succeeded {
            (
                TaskState::Skipped,
                html! { @if build.detail.as_deref().is_some_and(|d| d.starts_with("Shadow")) { "Shadow run" } @else { "Candidate only" } },
            )
        } else if build.state.is_finished() {
            (TaskState::Skipped, html! { "Skipped" })
        } else {
            (TaskState::Pending, html! { "Waiting" })
        };
        out.push(Stage { name: "Release", class: task_kind(state).0, state, info, first: None });
    }
    out
}

fn pipeline(data: &BuildData) -> Markup {
    let stages = stages(data);
    if stages.len() < 2 {
        return html! {};
    }
    html! {
        nav class="pipeline" aria-label="Pipeline" {
            @for (index, stage) in stages.iter().enumerate() {
                @if index > 0 { span class={"connector" @if stages[index - 1].state == TaskState::Passed { " done" }} {} }
                @let class = if matches!(stage.state, TaskState::Skipped | TaskState::Cancelled) { "skip" } else { stage.class };
                @match stage.first {
                    Some(task) => a class={"stage " (class)} href={"#task-" (task)} data-task=(task) data-attempt=(data.attempt) {
                        (task_status(stage.state, "sm"))
                        span class="what" { span class="name" { (stage.name) } span class="info" { (stage.info) } }
                    },
                    None => div class={"stage " (class)} {
                        (task_status(stage.state, "sm"))
                        span class="what" { span class="name" { (stage.name) } span class="info" { (stage.info) } }
                    },
                }
            }
        }
    }
}

fn trigger_url(ctx: &Context<'_>, repo: &Repo, build: &Build) -> String {
    let base = format!("{}/{}", ctx.github_web, repo.full_name());
    match build.trigger {
        Trigger::PullRequest => format!("{base}/pull/{}", build.pr_number.unwrap_or_default()),
        Trigger::Tag | Trigger::Push | Trigger::Manual => format!("{base}/tree/{}", build.ref_name),
    }
}

fn banner(build: &Build, ahead: Option<usize>) -> Markup {
    let detail = build.detail.clone().unwrap_or_default();
    let config = build.failure == Some(Failure::Config);
    let title = headline(build);
    let show_detail = (!detail.is_empty() && !title.contains(detail.lines().next().unwrap_or_default())) || config;
    let quiet = matches!(build.state, BuildState::Running | BuildState::Starting) && !show_detail;
    if quiet || (build.state == BuildState::Succeeded && !show_detail && title == "Passed") {
        return html! {};
    }
    let glyph = match build.state {
        BuildState::Succeeded if released(build) => Icon::Rocket,
        BuildState::Queued => Icon::Clock,
        BuildState::Failed | BuildState::Errored => Icon::Alert,
        state => state_icon(state),
    };
    html! {
        div class={"banner " (kind(build.state))} role=[matches!(build.state, BuildState::Failed | BuildState::Errored).then_some("alert")] {
            (icon(glyph))
            div class="text" {
                strong { (title) }
                @if show_detail {
                    @if config || detail.contains('\n') { pre { (detail) } } @else { span class="hint" { (detail) } }
                }
                @if build.state.is_active() && build.state != BuildState::Releasing && title != phase_label(build.phase) { span class="hint" { (phase_label(build.phase)) } }
                @if let Some(ahead) = ahead {
                    span class="hint" { (match ahead { 0 => "Next in line".to_owned(), 1 => "1 build ahead".to_owned(), n => format!("{n} builds ahead") }) }
                }
            }
        }
    }
}

pub fn sidebar(ctx: &Context<'_>, data: &BuildData, selected: Option<i64>) -> Markup {
    let BuildData { repo, build, .. } = data;
    html! {
        @if !data.tasks.is_empty() || !data.images.is_empty() {
            section class="card tasks" aria-label="Tasks" {
                @for (index, task) in data.tasks.iter().enumerate() {
                    @let group = phase_group(task.kind);
                    @if index == 0 || phase_group(data.tasks[index - 1].kind) != group {
                        div class="group-label" { (group) }
                    }
                    a class={"task" @if Some(task.ordinal) == selected { " selected" }} href={"#task-" (task.ordinal)}
                        data-task=(task.ordinal) data-attempt=(data.attempt) data-state=(task.state.as_str()) data-default[Some(task.ordinal) == selected] {
                        (task_status(task.state, "sm"))
                        span class="name" { (task_title(task)) }
                        span class="time" { (elapsed(task.started_at, task.finished_at)) }
                    }
                }
                @if data.images.iter().any(|i| i.released.is_some()) || build.state == BuildState::Releasing {
                    div class="group-label" { "Release" }
                    @for image in &data.images {
                        div class="task static" {
                            (task_status(if image.released.is_some() { TaskState::Passed } else { TaskState::Running }, "sm"))
                            span class="name" { (image.name) }
                            span class="time mono" { @if let Some(tag) = &image.released { (tag) } }
                        }
                    }
                }
            }
        }
        section class="card" {
            div class="card-head" { h2 { "Details" } }
            dl class="facts" {
                dt { "Status" } dd { span class={"pill " (kind(build.state))} { (state_label(build.state)) } }
                dt { "Repository" } dd { a href=(repo_path(repo)) { (repo.full_name()) } }
                dt { "Trigger" } dd {
                    (match build.trigger { Trigger::Push => "Push", Trigger::Tag => "Tag", Trigger::PullRequest => "Pull request", Trigger::Manual => "Manual" })
                    a href=(trigger_url(ctx, repo, build)) target="_blank" rel="noopener" { @if build.trigger == Trigger::PullRequest { "#" (build.pr_number.unwrap_or_default()) } @else { (build.ref_name) } }
                }
                dt { "Commit" } dd { a class="mono" href={(ctx.github_web) "/" (repo.full_name()) "/commit/" (build.sha)} target="_blank" rel="noopener" { (build.short_sha()) } }
                @if let Some(version) = &build.version { dt { "Version" } dd { code { (version) } } }
                @if let Some(actor) = &build.actor { dt { "Started by" } dd { (actor) } }
                dt { "Created" } dd { (time(build.created_at)) }
                @if let Some(started) = build.started_at { dt { "Waited" } dd { (duration(started - build.created_at)) } }
                @if build.started_at.is_some() { dt { "Duration" } dd { (elapsed(build.started_at, build.finished_at)) } }
                @if build.attempt > 1 {
                    dt { "Attempt" } dd {
                        div class="attempts" {
                            @for attempt in 1..=build.attempt {
                                @if attempt == data.attempt { span class="current" aria-current="true" { (attempt) } } @else { a href={(build_path(repo, build)) "?attempt=" (attempt)} { (attempt) } }
                            }
                        }
                    }
                }
                @if let Some(original) = &data.rerun_of { dt { "Rerun of" } dd { a href=(build_path(repo, original)) { "#" (original.number) } } }
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
            div class="card-head" { h2 { "Images" span class="count" { (images.len()) } } }
            div class="table-wrap" {
                table class="table" {
                    thead { tr { th { "Image" } th { "Digest" } th { "Released as" } } }
                    tbody {
                        @for image in images {
                            tr {
                                td { div class="cell" { span class="muted" { (icon(Icon::Box)) } strong { (image.name) } } }
                                td {
                                    @match &image.digest {
                                        Some(digest) => span class="digest" {
                                            code title=(digest) { (&digest[..digest.len().min(19)]) }
                                            button class="copy" type="button" data-copy=(digest) title="Copy digest" aria-label="Copy digest" { (icon(Icon::Copy)) }
                                        },
                                        None => span class="muted" { "Not built" },
                                    }
                                }
                                td { @match &image.released { Some(tag) => span class="pill mono ok" { (icon(Icon::Tag)) (tag) }, None => span class="muted" { "–" } } }
                            }
                        }
                    }
                }
            }
        }
    }
}

pub fn log_view(build: &Build, attempt: i64, task: &Task, text: &str, bytes: usize, skipped: Option<usize>) -> Markup {
    let raw = format!("/builds/{}/logs/{attempt}/{}.txt", build.id, task.ordinal);
    html! {
        div class="log" {
            div class="log-head" data-bytes=(bytes) data-open=(!text.is_empty() && !text.ends_with('\n')) data-first-line=(skipped.unwrap_or(0)) data-state=(task.state.as_str()) {
                div class="name" { (task_status(task.state, "sm")) span { (task_title(task)) } }
                span class="meta" {
                    (elapsed(task.started_at, task.finished_at))
                    @if let Some(code) = task.exit_code.filter(|c| *c != 0) { " · exit " (code) }
                }
                div class="log-tools" {
                    label class="log-find" {
                        (icon(Icon::Search))
                        span class="sr-only" { "Find in log" }
                        input type="search" placeholder="Find in log" data-find autocomplete="off" spellcheck="false";
                        span class="hits" data-hits {}
                    }
                    button class="log-tool" type="button" data-wrap title="Wrap long lines" aria-pressed="true" { (icon(Icon::Wrap)) }
                    button class="log-tool" type="button" data-expand title="Expand all steps" { (icon(Icon::Expand)) }
                    @if task.state == TaskState::Running { button class="log-tool" type="button" data-follow title="Keep the newest output in view" { (icon(Icon::ArrowDown)) "Follow" } }
                    a class="log-tool" href=(raw) download title="Download the raw log" { (icon(Icon::Download)) }
                }
            }
            div class="log-body" data-task=(task.ordinal) {
                @if let Some(lines) = skipped {
                    div class="log-empty" { (lines) " earlier lines hidden · " a href=(raw) download { "Full log" } }
                }
                @if text.trim().is_empty() {
                    div class="log-empty" { @if task.state == TaskState::Pending { "Not started" } @else if task.state == TaskState::Skipped { "Skipped" } @else if task.state == TaskState::Running { "Waiting for output…" } @else { "No output" } }
                } @else {
                    (render_log(task, text, skipped.unwrap_or(0)))
                }
            }
        }
    }
}

fn render_log(task: &Task, text: &str, first: usize) -> Markup {
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
    let mut number = first;
    let mut numbered = |lines: &[&str]| -> Markup {
        html! { div class="lines" { @for line in lines { (ansi::line(task.ordinal, { number += 1; number }, line)) } } }
    };
    let preamble_markup = (!preamble.is_empty() || sections.is_empty()).then(|| numbered(&preamble));
    let section_markup: Vec<(usize, Markup)> =
        sections.iter().map(|(index, lines)| (*index, numbered(lines))).collect();
    html! {
        @if let Some(markup) = preamble_markup { (markup) }
        @for (position, (index, lines)) in section_markup.into_iter().enumerate() {
            @let step = task.steps.iter().find(|s| s.ordinal == index as i64);
            @let state = step.map(|s| s.state).unwrap_or(TaskState::Running);
            @let open = matches!(state, TaskState::Running | TaskState::Failed | TaskState::TimedOut);
            details open[open] data-open-last[position + 1 == sections.len() && open] {
                summary {
                    (icon(Icon::Chevron))
                    span class={"dot " (task_kind(state).0)} {}
                    span class="name" { (step.map(|s| s.name.clone()).unwrap_or_else(|| format!("Step {}", index + 1))) }
                    span class="time" { @if let Some(step) = step { (elapsed(step.started_at, step.finished_at)) } }
                }
                (lines)
            }
        }
    }
}

pub struct SystemData {
    pub facts: Vec<(&'static str, String)>,
    pub audit: Vec<(AuditEntry, Option<(String, String)>)>,
    pub active: usize,
    pub slots: usize,
    pub enrolled: usize,
    pub visible: usize,
}

fn action(action: &str) -> (&'static str, Icon, &'static str) {
    match action {
        "build_triggered" => ("started a build", Icon::Play, ""),
        "build_cancelled" => ("cancelled a build", Icon::Stop, ""),
        "build_rerun" => ("reran a build", Icon::Rerun, ""),
        "image_released" => ("released", Icon::Rocket, "ok"),
        "cache_reset" => ("reset the build cache of", Icon::Trash, ""),
        "registry_drift" => ("found versions published outside Kiln in", Icon::Alert, "warn"),
        "registry_drift_dismissed" => ("dismissed the registry warning of", Icon::Check, ""),
        "repository_enrolled" => ("started building", Icon::Book, "ok"),
        "repository_paused" => ("paused", Icon::Pause, "warn"),
        "repository_resumed" => ("resumed", Icon::Play, ""),
        "login" => ("signed in", Icon::User, ""),
        "login_denied" => ("was refused sign-in", Icon::Alert, "bad"),
        "registry_token_saved" => ("saved the registry token for", Icon::Key, ""),
        "person_added" => ("gave access to", Icon::Users, "ok"),
        "person_removed" => ("removed access for", Icon::Users, "warn"),
        "kiln_claimed" => ("claimed Kiln", Icon::Key, "ok"),
        "github_disconnected" => ("disconnected GitHub", Icon::Alert, "warn"),
        _ => ("", Icon::Activity, ""),
    }
}

pub fn system(ctx: &Context<'_>, data: &SystemData) -> Markup {
    let github = data.facts.iter().find(|(k, _)| *k == "GitHub API").map(|(_, v)| v.clone());
    layout(
        "System",
        "system",
        ctx,
        &[("page", "system".into())],
        html! {
            div class="page-head" {
                div class="titles" { h1 { "System" } }
                div class="actions" { (refresh_button()) }
            }
            div class="tiles" {
                div class="card tile" {
                    span class="label" { (icon(Icon::Layers)) "Version" }
                    span class="value mono" { (VERSION) }
                    span class="foot" { @if ctx.demo { "Demo mode" } @else { "Running" } }
                }
                div class="card tile" {
                    span class="label" { (icon(Icon::Gauge)) "Build slots" }
                    span class="value" { (data.active) small { "of " (data.slots) " busy" } }
                    div class="meter" { @for slot in 0..data.slots { span class=(if slot < data.active { "on" } else { "" }) {} } }
                }
                a class="card tile" href="/repos" {
                    span class="label" { (icon(Icon::Book)) "Repositories" }
                    span class="value" { (data.enrolled) small { "of " (data.visible) " use Kiln" } }
                }
                div class="card tile" {
                    span class="label" { (PreEscaped(GITHUB)) "GitHub API" }
                    span class="value" style="font-size:15px" { (github.unwrap_or_else(|| "–".into())) }
                }
            }
            div class="columns" {
                section class="card" {
                    div class="card-head" { h2 { "Activity" } }
                    @if data.audit.is_empty() {
                        (empty(Icon::Activity, "Nothing recorded yet", html! {}))
                    } @else {
                        ul class="activity" {
                            @for (entry, target) in &data.audit {
                                @let (verb, glyph, tone) = action(&entry.action);
                                li {
                                    span class={"ico " (tone)} { (icon(glyph)) }
                                    div class="what" {
                                        strong { (entry.actor) } " "
                                        @match (entry.action.as_str(), entry.detail.as_deref().and_then(|d| d.split_once(' '))) {
                                            ("image_released", Some((reference, digest))) => {
                                                "released " code { (reference.rsplit('/').next().unwrap_or(reference)) }
                                                @if let Some((link, label)) = target { " from " a href=(link) { (label) } }
                                                span class="detail mono" title=(digest) { (&digest[..digest.len().min(19)]) }
                                            },
                                            _ => {
                                                @if verb.is_empty() { (entry.action.replace('_', " ")) } @else { (verb) }
                                                @if let Some((link, label)) = target { " " a href=(link) { (label) } }
                                                @if let Some(detail) = &entry.detail { span class="detail" { (detail) } }
                                            },
                                        }
                                    }
                                    span class="when" { (time(entry.at)) }
                                }
                            }
                        }
                    }
                }
                aside class="card" {
                    div class="card-head" { h2 { "Configuration" } }
                    dl class="facts" { @for (key, value) in data.facts.iter().filter(|(k, _)| *k != "GitHub API") { dt { (key) } dd { (value) } } }
                }
            }
        },
    )
}

pub struct SettingsData {
    pub github: Option<kiln_store::GitHubApp>,
    pub people: Vec<kiln_store::Person>,
    pub registry: String,
    pub registry_username: String,
    pub has_token: bool,
    pub expires: Option<i64>,
    pub secrets_error: Option<String>,
    pub you: String,
}

pub fn settings(ctx: &Context<'_>, data: &SettingsData) -> Markup {
    layout(
        "Settings",
        "settings",
        ctx,
        &[],
        html! {
            div class="page-head" { div class="titles" { h1 { "Settings" } } }
            div class="settings-grid" {
                section class="card" {
                    div class="card-head" { h2 { (PreEscaped(GITHUB)) "GitHub" } }
                    @match &data.github {
                        Some(app) => {
                            dl class="facts" {
                                dt { "App" } dd { a href=(app.html_url) target="_blank" rel="noopener" { (app.name) } }
                                dt { "Account" } dd { (app.owner_login) }
                                dt { "Connected" } dd { (time(app.connected_at)) }
                            }
                            div class="card-foot start" {
                                a class="button" href={(app.html_url.trim_end_matches('/')) "/installations/new"} target="_blank" rel="noopener" { (icon(Icon::Book)) "Choose repositories" }
                                form method="post" action="/settings/disconnect"
                                    data-confirm="Kiln stops building and shows Connect GitHub. People and history stay; delete the old app on GitHub afterwards."
                                    data-confirm-title="Disconnect GitHub?" data-confirm-action="Disconnect" data-confirm-danger {
                                    button class="button danger" type="submit" { "Disconnect" }
                                }
                            }
                        },
                        None => div class="empty compact" { p { "Not connected" } },
                    }
                }
                section class="card" id="registry" {
                    div class="card-head" {
                        h2 { (icon(Icon::Key)) "Registry" }
                        @if data.secrets_error.is_some() { span class="pill bad" { "Secrets failed" } }
                        @else if let (true, Some(note)) = (data.has_token, &ctx.registry_note) { span class="pill warn" { (note) } }
                        @else if data.has_token { span class="pill ok" { "Token saved" } }
                        @else { span class="pill warn" { "No token" } }
                    }
                    form class="settings-form" method="post" action="/settings/registry"
                        data-confirm="Kiln restarts to use the new token. Running builds continue." data-confirm-title="Save the registry token?" data-confirm-action="Save" {
                        @if let Some(error) = &data.secrets_error {
                            div class="banner bad" { (icon(Icon::Alert)) div class="text" { strong { "Build pods cannot get the token" } span class="hint" { (error) } } }
                        }
                        dl class="facts" {
                            dt { "Publishes to" } dd { code { (data.registry) } }
                            @if data.has_token {
                                dt { "Token of" } dd { (data.registry_username) }
                                dt { "Expires" } dd { @if let Some(at) = data.expires { (time(at)) } @else { "Never" } }
                            }
                        }
                        @if data.registry.starts_with("ghcr.io/") {
                            ol class="token-steps" {
                                li {
                                    span class="num" { "1" }
                                    div class="what" { strong { "Create a classic token" } span class="hint" { "Scope " code { "write:packages" } } }
                                    a class="button small" href={(ctx.github_web) "/settings/tokens/new?scopes=write:packages&description=Kiln"} target="_blank" rel="noopener" { (PreEscaped(GITHUB)) "Create token" }
                                }
                                li {
                                    span class="num" { "2" }
                                    label class="what" {
                                        strong { "Paste it" }
                                        input class="input" type="password" name="token" autocomplete="new-password" required spellcheck="false" placeholder=(if data.has_token { "Replaces the saved token" } else { "ghp_…" });
                                    }
                                }
                            }
                        } @else {
                            label class="field" { span { "Username" } input class="input" name="username" value=(data.registry_username) autocomplete="off" required; }
                            label class="field" {
                                span { "Token" }
                                input class="input" type="password" name="token" autocomplete="new-password" required placeholder=(if data.has_token { "Replaces the saved token" } else { "" });
                            }
                        }
                        div class="actions" { button class="button primary" type="submit" disabled[ctx.demo] { "Save token" } }
                    }
                }
                section class="card wide" {
                    div class="card-head" { h2 { (icon(Icon::Users)) "People" span class="count" { (data.people.len()) } } }
                    ul class="people" {
                        @for person in &data.people {
                            li {
                                span class="avatar small" { (initial(&person.login)) }
                                div class="who" {
                                    strong { (person.login) @if person.login == data.you { span class="muted" { " · you" } } }
                                    span class="muted" { "Added by " (person.added_by) " · " (time(person.added_at)) }
                                }
                                @if data.people.len() > 1 {
                                    form method="post" action={"/settings/people/" (person.user_id) "/remove"}
                                        data-confirm={(person.login) " can no longer sign in."} data-confirm-title={"Remove " (person.login) "?"} data-confirm-action="Remove" data-confirm-danger {
                                        button class="button small danger" type="submit" { "Remove" }
                                    }
                                }
                            }
                        }
                    }
                    form class="add-person" method="post" action="/settings/people" {
                        input class="input" name="login" placeholder="GitHub username" autocomplete="off" spellcheck="false" required;
                        button class="button" type="submit" { "Add" }
                    }
                }
            }
        },
    )
}
