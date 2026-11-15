mod ansi;
pub mod connect;
mod views;

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, FromRequestParts, Path, Query, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_DISPOSITION, CONTENT_TYPE, ORIGIN};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use futures::{Stream, StreamExt};
use hmac::{Hmac, KeyInit, Mac};
use kiln_core::config;
use kiln_github::{OAuthApp, unix_now};
use kiln_store::{BuildFilter, BuildState, Repo, Session, Trigger};
use maud::Markup;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tower_http::set_header::SetResponseHeaderLayer;

use super::engine::Engine;
use super::platform::Platform;
use views::{Context, Viewer};

const SESSION_COOKIE: &str = "__Host-kiln_session";
const OAUTH_COOKIE: &str = "__Host-kiln_oauth";
pub const CLAIM_COOKIE: &str = "__Host-kiln_claim";
const PENDING_COOKIE: &str = "__Host-kiln_pending";
const SESSION_IDLE: i64 = 12 * 3600;
const SESSION_MAX: i64 = 7 * 86_400;
fn content_policy(github_web: &str) -> HeaderValue {
    let avatars = match url::Url::parse(github_web) {
        Ok(url) if url.host_str() == Some("github.com") => "https://avatars.githubusercontent.com".to_owned(),
        Ok(url) => {
            let host = url.host_str().unwrap_or_default();
            format!("{} {}://avatars.{host}", url.origin().ascii_serialization(), url.scheme())
        }
        Err(_) => "https://avatars.githubusercontent.com".to_owned(),
    };
    let policy = format!(
        "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' {avatars}; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'"
    );
    HeaderValue::from_str(&policy).unwrap_or_else(|_| HeaderValue::from_static("default-src 'self'"))
}

pub struct Web<P: Platform> {
    pub engine: Arc<Engine<P>>,
    pub oauth: Option<OAuthApp>,
    pub demo: bool,
    pub claim_code: Option<String>,
    pub claims: std::sync::Mutex<std::collections::HashMap<String, (kiln_github::GitHubUser, i64)>>,
    pub deliveries: std::sync::Mutex<std::collections::VecDeque<String>>,
}

pub fn router<P: Platform>(web: Arc<Web<P>>) -> Router {
    let policy = content_policy(&web.engine.settings.github_web);
    Router::new()
        .route("/", get(dashboard::<P>))
        .route("/repos", get(repos::<P>))
        .route("/system", get(system::<P>))
        .route("/system/refresh", post(refresh::<P>))
        .route("/settings", get(settings_page::<P>))
        .route("/settings/registry", post(save_registry::<P>))
        .route("/settings/disconnect", post(disconnect::<P>))
        .route("/settings/people", post(add_person::<P>))
        .route("/settings/people/{id}/remove", post(remove_person::<P>))
        .route("/events", get(events::<P>))
        .route("/fragments/ping", get(|_user: User| async { StatusCode::NO_CONTENT }))
        .route("/fragments/dashboard", get(fragment_dashboard::<P>))
        .route("/fragments/repos", get(fragment_repos::<P>))
        .route("/fragments/repo/{id}", get(fragment_repo::<P>))
        .route("/fragments/build/{id}", get(fragment_build::<P>))
        .route("/fragments/tasks/{id}", get(fragment_tasks::<P>))
        .route("/fragments/log/{build}/{attempt}/{task}", get(fragment_log::<P>))
        .route("/builds/{build}/logs/{attempt}/{task}", get(raw_log::<P>))
        .route("/login", get(login::<P>))
        .route("/claim", get(claim_page::<P>).post(claim::<P>))
        .route("/auth/github", get(auth_start::<P>))
        .route("/auth/callback", get(auth_callback::<P>))
        .route("/logout", post(logout::<P>))
        .route("/hooks/github", post(webhook::<P>))
        .route("/healthz", get(|| async { "ok" }))
        .route("/readyz", get(readyz::<P>))
        .route("/assets/kiln.css", get(|| async { asset("text/css; charset=utf-8", include_str!("assets/kiln.css")) }))
        .route(
            "/assets/kiln.js",
            get(|| async { asset("text/javascript; charset=utf-8", include_str!("assets/kiln.js")) }),
        )
        .route("/assets/favicon.svg", get(|| async { asset("image/svg+xml", include_str!("assets/favicon.svg")) }))
        .route("/assets/favicon-{state}", get(favicon))
        .route("/{owner}/{repo}", get(repo_page::<P>))
        .route("/{owner}/{repo}/build", post(trigger::<P>))
        .route("/{owner}/{repo}/pause", post(pause::<P>))
        .route("/{owner}/{repo}/resume", post(resume::<P>))
        .route("/{owner}/{repo}/reset-cache", post(reset_cache::<P>))
        .route("/{owner}/{repo}/dismiss-drift", post(dismiss_drift::<P>))
        .route("/{owner}/{repo}/builds/{number}", get(build_page::<P>))
        .route("/{owner}/{repo}/builds/{number}/cancel", post(cancel::<P>))
        .route("/{owner}/{repo}/builds/{number}/rerun", post(rerun::<P>))
        .layer(DefaultBodyLimit::max(5 * 1024 * 1024))
        .layer(SetResponseHeaderLayer::overriding(axum::http::header::CONTENT_SECURITY_POLICY, policy))
        .layer(SetResponseHeaderLayer::overriding(
            axum::http::header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            axum::http::header::REFERRER_POLICY,
            HeaderValue::from_static("same-origin"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            axum::http::header::X_FRAME_OPTIONS,
            HeaderValue::from_static("DENY"),
        ))
        .with_state(web)
}

async fn favicon(Path(state): Path<String>) -> Response {
    let color = match state.trim_end_matches(".svg") {
        "ok" => "#16a34a",
        "bad" => "#dc2626",
        "run" => "#2563eb",
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    let body = include_str!("assets/favicon.svg").replace(
        "</svg>",
        &format!(r##"<circle cx="19" cy="19" r="4.5" fill="{color}" stroke="#fff" stroke-width="1.5"/></svg>"##),
    );
    ([(CONTENT_TYPE, "image/svg+xml"), (CACHE_CONTROL, "public, max-age=86400")], body).into_response()
}

fn asset(kind: &'static str, body: &'static str) -> Response {
    ([(CONTENT_TYPE, kind), (CACHE_CONTROL, "public, max-age=31536000, immutable")], body).into_response()
}

pub enum WebError {
    SignIn(String),
    SignedOut,
    NotFound(String),
    Forbidden(String),
    BadRequest(String),
    Internal(anyhow::Error),
}

impl From<anyhow::Error> for WebError {
    fn from(error: anyhow::Error) -> Self {
        WebError::Internal(error)
    }
}

impl From<kiln_store::Error> for WebError {
    fn from(error: kiln_store::Error) -> Self {
        WebError::Internal(error.into())
    }
}

impl IntoResponse for WebError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            WebError::SignIn(next) => {
                return Redirect::to(&format!("/login?next={}", urlencode(&next))).into_response();
            }
            WebError::SignedOut => return StatusCode::UNAUTHORIZED.into_response(),
            WebError::NotFound(message) => (StatusCode::NOT_FOUND, message),
            WebError::Forbidden(message) => (StatusCode::FORBIDDEN, message),
            WebError::BadRequest(message) => (StatusCode::BAD_REQUEST, message),
            WebError::Internal(error) => {
                tracing::error!(error = format!("{error:#}"), "request failed");
                (StatusCode::INTERNAL_SERVER_ERROR, "Something went wrong.".to_owned())
            }
        };
        (status, views::error_page(None, status.as_u16(), &message)).into_response()
    }
}

type WebResult<T> = Result<T, WebError>;

fn urlencode(text: &str) -> String {
    serde_urlencoded::to_string([("v", text)]).map(|s| s[2..].to_owned()).unwrap_or_default()
}

fn hash(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

pub struct User {
    pub id: i64,
    pub login: String,
    pub avatar_url: String,
    pub session: String,
}

impl<P: Platform> FromRequestParts<Arc<Web<P>>> for User {
    type Rejection = WebError;

    async fn from_request_parts(parts: &mut Parts, web: &Arc<Web<P>>) -> Result<Self, Self::Rejection> {
        let next = parts.uri.path_and_query().map(|p| p.as_str().to_owned()).unwrap_or_else(|| "/".into());
        let jar = CookieJar::from_headers(&parts.headers);
        let Some(token) = jar.get(SESSION_COOKIE).map(|c| c.value().to_owned()) else {
            return Err(sign_in(parts, next));
        };
        let id_hash = hash(&token);
        let store = &web.engine.store;
        let Some(session) = store.session(&id_hash)? else { return Err(sign_in(parts, next)) };
        let now = unix_now();
        let allowed = web.demo || store.is_person(session.user_id)?;
        if !allowed || now - session.seen_at > SESSION_IDLE || now - session.created_at > SESSION_MAX {
            store.delete_session(&id_hash)?;
            return Err(sign_in(parts, next));
        }
        if now - session.seen_at > 60 {
            store.touch_session(&id_hash, now)?;
        }
        Ok(User { id: session.user_id, login: session.login, avatar_url: session.avatar_url, session: id_hash })
    }
}

fn sign_in(parts: &Parts, next: String) -> WebError {
    if parts.headers.contains_key("x-kiln-fragment") || parts.uri.path().starts_with("/events") {
        return WebError::SignedOut;
    }
    WebError::SignIn(next)
}

fn same_origin<P: Platform>(web: &Web<P>, headers: &HeaderMap) -> WebResult<()> {
    let expected = web.engine.settings.public_url.as_str();
    let origin = headers.get(ORIGIN).and_then(|v| v.to_str().ok()).unwrap_or_default();
    if origin == expected { Ok(()) } else { Err(WebError::Forbidden("Request from another site.".into())) }
}

fn context<'a, P: Platform>(web: &'a Web<P>, viewer: &'a Viewer) -> Context<'a> {
    Context {
        viewer,
        github_web: &web.engine.settings.github_web,
        demo: web.demo,
        registry_note: registry_note(&web.engine.settings),
    }
}

fn registry_note(settings: &crate::server::Settings) -> Option<String> {
    if settings.registry_password.is_none() {
        return Some("Releases wait for a registry token".into());
    }
    let days = (settings.registry_expires? - kiln_github::unix_now() as i64) / 86_400;
    match days {
        ..0 => Some("The registry token has expired".into()),
        0 => Some("The registry token expires today".into()),
        1..=13 => Some(format!("The registry token expires in {days} days")),
        _ => None,
    }
}

fn viewer(user: &User) -> Viewer {
    Viewer { login: user.login.clone(), avatar_url: user.avatar_url.clone() }
}

fn flash(path: &str, message: &str) -> Response {
    let mut note = Cookie::new("kiln_flash", message.to_owned());
    note.set_path("/");
    note.set_secure(true);
    note.set_same_site(SameSite::Strict);
    note.set_max_age(time::Duration::seconds(30));
    (CookieJar::new().add(note), Redirect::to(path)).into_response()
}

fn safe_next(path: &str) -> bool {
    path.starts_with('/')
        && !path.starts_with("//")
        && !path.contains('\\')
        && !path.chars().any(|c| c.is_control() || c.is_whitespace())
}

fn render(markup: Markup) -> Response {
    ([(CACHE_CONTROL, "no-store")], markup).into_response()
}

fn repo_named<P: Platform>(web: &Web<P>, owner: &str, name: &str) -> WebResult<Repo> {
    web.engine
        .store
        .repo_by_name(owner, name)?
        .ok_or_else(|| WebError::NotFound(format!("Kiln does not know {owner}/{name}.")))
}

fn latest<P: Platform>(web: &Web<P>, repos: &[Repo]) -> WebResult<Vec<views::RepoRow>> {
    let store = &web.engine.store;
    let mut out = Vec::new();
    for repo in repos {
        let builds = store.builds(&BuildFilter { repo: Some(repo.id), limit: 40, ..Default::default() })?;
        let last = builds.iter().find(|b| b.trigger != Trigger::PullRequest).cloned();
        let version = builds
            .iter()
            .find(|b| {
                b.state == BuildState::Succeeded && b.detail.as_deref().is_some_and(|d| d.starts_with("Released"))
            })
            .and_then(|b| b.version.clone());
        let mut recent: Vec<_> = builds.into_iter().take(12).collect();
        recent.reverse();
        out.push(views::RepoRow { repo: repo.clone(), last, version, recent });
    }
    out.sort_by(|a, b| {
        let at = |row: &views::RepoRow| row.recent.last().map(|b| b.created_at).unwrap_or(0);
        at(b).cmp(&at(a)).then_with(|| a.repo.name.cmp(&b.repo.name))
    });
    Ok(out)
}

#[derive(Deserialize, Default)]
struct DashboardQuery {
    status: Option<String>,
    repo: Option<String>,
    before: Option<i64>,
}

fn live_info<P: Platform>(web: &Web<P>, build: &kiln_store::Build, history: &[kiln_store::Build]) -> views::Live {
    let pull_request = build.trigger == Trigger::PullRequest;
    let similar: Vec<kiln_store::Build> = history
        .iter()
        .filter(|b| b.repo_id == build.repo_id && (b.trigger == Trigger::PullRequest) == pull_request)
        .take(20)
        .cloned()
        .collect();
    views::Live { estimate: views::median(&similar), samples: web.engine.usage.history(build.id) }
}

fn dashboard_data<P: Platform>(web: &Web<P>, query: &DashboardQuery) -> WebResult<views::DashboardData> {
    let store = &web.engine.store;
    let page = 30;
    let repos = store.repos()?;
    let mut active = store.active()?;
    active.reverse();
    let mut queued = store.queued()?;
    queued.reverse();
    let status = views::Status::parse(query.status.as_deref());
    let repo = query.repo.as_deref().and_then(|r| r.parse().ok()).filter(|id| repos.iter().any(|r| r.id == *id));
    let mut builds = store.builds(&BuildFilter {
        repo,
        before: query.before,
        states: status.states(),
        limit: page + 1,
        ..Default::default()
    })?;
    let more = (builds.len() as i64 > page).then(|| builds[page as usize - 1].id);
    builds.truncate(page as usize);
    let finished =
        store.builds(&BuildFilter { states: views::Status::All.states(), limit: 200, ..Default::default() })?;
    let day = unix_now() - 86_400;
    let today = finished.iter().filter(|b| b.finished_at.unwrap_or(b.created_at) >= day);
    let judged: Vec<_> = finished
        .iter()
        .filter(|b| matches!(b.state, BuildState::Succeeded | BuildState::Failed | BuildState::Errored))
        .take(50)
        .cloned()
        .collect();
    let stats = views::Stats {
        passed_day: today.clone().filter(|b| b.state == BuildState::Succeeded).count(),
        failed_day: today.filter(|b| matches!(b.state, BuildState::Failed | BuildState::Errored)).count(),
        rate: (!judged.is_empty())
            .then(|| (judged.iter().filter(|b| b.state == BuildState::Succeeded).count(), judged.len())),
        median: views::median(&judged),
        hours: {
            let mut hours = vec![(0, 0); 24];
            for build in &finished {
                let at = build.finished_at.unwrap_or(build.created_at);
                let age = (unix_now() - at).max(0) / 3600;
                if age < 24 {
                    let slot = &mut hours[23 - age as usize];
                    match build.state {
                        BuildState::Succeeded => slot.0 += 1,
                        BuildState::Failed | BuildState::Errored => slot.1 += 1,
                        _ => {}
                    }
                }
            }
            hours
        },
        recent: judged.iter().take(30).rev().map(|b| b.state).collect(),
    };
    let live = active.iter().map(|b| (b.id, live_info(web, b, &finished))).collect();
    Ok(views::DashboardData {
        live,
        repos,
        active,
        queued,
        builds,
        more,
        status,
        repo,
        slots: web.engine.settings.slots,
        stats,
    })
}

async fn dashboard<P: Platform>(
    State(web): State<Arc<Web<P>>>,
    user: User,
    Query(query): Query<DashboardQuery>,
) -> WebResult<Response> {
    let viewer = viewer(&user);
    Ok(render(views::dashboard(&context(&web, &viewer), &dashboard_data(&web, &query)?)))
}

async fn fragment_dashboard<P: Platform>(
    State(web): State<Arc<Web<P>>>,
    _user: User,
    Query(query): Query<DashboardQuery>,
) -> WebResult<Response> {
    Ok(render(views::dashboard_live(&dashboard_data(&web, &query)?)))
}

async fn repos<P: Platform>(State(web): State<Arc<Web<P>>>, user: User) -> WebResult<Response> {
    let viewer = viewer(&user);
    let repos = web.engine.store.repos()?;
    Ok(render(views::repos(&context(&web, &viewer), &latest(&web, &repos)?)))
}

async fn fragment_repos<P: Platform>(State(web): State<Arc<Web<P>>>, _user: User) -> WebResult<Response> {
    let repos = web.engine.store.repos()?;
    Ok(render(views::repos_live(&latest(&web, &repos)?)))
}

#[derive(Deserialize, Default)]
struct Paging {
    before: Option<i64>,
    show: Option<String>,
}

fn repo_data<P: Platform>(web: &Web<P>, repo: Repo, paging: &Paging) -> WebResult<views::RepoData> {
    let store = &web.engine.store;
    let page = 30;
    let show = views::Show::parse(paging.show.as_deref());
    let mut builds = store.builds(&BuildFilter {
        repo: Some(repo.id),
        before: paging.before,
        triggers: show.triggers(),
        limit: page + 1,
        ..Default::default()
    })?;
    let more = (builds.len() as i64 > page).then(|| builds[page as usize - 1].id);
    builds.truncate(page as usize);
    let releases = store
        .builds(&BuildFilter {
            repo: Some(repo.id),
            states: vec![BuildState::Succeeded],
            limit: 100,
            ..Default::default()
        })?
        .into_iter()
        .filter(|b| b.detail.as_deref().is_some_and(|d| d.starts_with("Released")))
        .filter_map(|b| Some((b.version.clone()?, b.number, b.finished_at.unwrap_or(b.created_at))))
        .take(8)
        .collect();
    let config = store
        .builds(&BuildFilter { repo: Some(repo.id), limit: 20, with_config: true, ..Default::default() })?
        .into_iter()
        .filter(|b| b.trigger != Trigger::PullRequest)
        .find_map(|b| b.config.and_then(|text| config::parse(&text, &repo.name).ok()));
    let busy = store.active()?.iter().any(|b| b.repo_id == repo.id);
    let mut history: Vec<_> = store
        .builds(&BuildFilter {
            repo: Some(repo.id),
            states: views::Status::All.states(),
            limit: 40,
            ..Default::default()
        })?
        .into_iter()
        .filter(|b| !matches!(b.state, BuildState::Skipped | BuildState::Superseded))
        .collect();
    history.reverse();
    let secret = web.engine.settings.repo_secret(&repo.full_name());
    Ok(views::RepoData { repo, builds, history, releases, config, more, busy, show, secret })
}

async fn repo_page<P: Platform>(
    State(web): State<Arc<Web<P>>>,
    user: User,
    Path((owner, name)): Path<(String, String)>,
    Query(paging): Query<Paging>,
) -> WebResult<Response> {
    let viewer = viewer(&user);
    let repo = repo_named(&web, &owner, &name)?;
    let data = repo_data(&web, repo, &paging)?;
    Ok(render(views::repo(&context(&web, &viewer), &data)))
}

async fn fragment_repo<P: Platform>(
    State(web): State<Arc<Web<P>>>,
    user: User,
    Path(id): Path<i64>,
    Query(paging): Query<Paging>,
) -> WebResult<Response> {
    let viewer = viewer(&user);
    let repo = web.engine.store.repo(id)?.ok_or_else(|| WebError::NotFound("repository not found".into()))?;
    let data = repo_data(&web, repo, &paging)?;
    Ok(render(views::repo_live(&context(&web, &viewer), &data)))
}

#[derive(Deserialize, Default)]
struct AttemptQuery {
    attempt: Option<i64>,
}

fn build_data<P: Platform>(
    web: &Web<P>,
    build: kiln_store::Build,
    attempt: Option<i64>,
) -> WebResult<views::BuildData> {
    let store = &web.engine.store;
    let repo = store.repo(build.repo_id)?.ok_or_else(|| WebError::NotFound("repository not found".into()))?;
    let attempt = attempt.filter(|a| (1..=build.attempt).contains(a)).unwrap_or(build.attempt);
    let tasks = store.tasks(build.id, attempt)?;
    let images = store.images(build.id)?;
    let rerun_of = match build.rerun_of {
        Some(id) => store.build(id)?,
        None => None,
    };
    let ahead = (build.state == BuildState::Queued).then(|| {
        let rank =
            |b: &kiln_store::Build| (b.trigger == Trigger::PullRequest, b.trigger != Trigger::Tag, b.created_at, b.id);
        store.queued().map(|queued| queued.iter().filter(|other| rank(other) < rank(&build)).count()).unwrap_or(0)
    });
    let live = build.state.is_active().then(|| {
        let history = store
            .builds(&BuildFilter {
                repo: Some(build.repo_id),
                states: vec![BuildState::Succeeded],
                limit: 40,
                ..Default::default()
            })
            .unwrap_or_default();
        live_info(web, &build, &history)
    });
    Ok(views::BuildData { ahead, repo, build, attempt, tasks, images, rerun_of, live })
}

async fn build_page<P: Platform>(
    State(web): State<Arc<Web<P>>>,
    user: User,
    Path((owner, name, number)): Path<(String, String, i64)>,
    Query(query): Query<AttemptQuery>,
) -> WebResult<Response> {
    let viewer = viewer(&user);
    let repo = repo_named(&web, &owner, &name)?;
    let build = web
        .engine
        .store
        .build_by_number(repo.id, number)?
        .ok_or_else(|| WebError::NotFound(format!("{} has no build #{number}.", repo.full_name())))?;
    let data = build_data(&web, build, query.attempt)?;
    Ok(render(views::build(&context(&web, &viewer), &data)))
}

async fn fragment_build<P: Platform>(
    State(web): State<Arc<Web<P>>>,
    user: User,
    Path(id): Path<i64>,
    Query(query): Query<AttemptQuery>,
) -> WebResult<Response> {
    let viewer = viewer(&user);
    let build = web.engine.store.build(id)?.ok_or_else(|| WebError::NotFound("build not found".into()))?;
    let data = build_data(&web, build, query.attempt)?;
    Ok(render(views::build_head(&context(&web, &viewer), &data)))
}

async fn fragment_tasks<P: Platform>(
    State(web): State<Arc<Web<P>>>,
    user: User,
    Path(id): Path<i64>,
    Query(query): Query<AttemptQuery>,
) -> WebResult<Response> {
    let build = web.engine.store.build(id)?.ok_or_else(|| WebError::NotFound("build not found".into()))?;
    let data = build_data(&web, build, query.attempt)?;
    let viewer = viewer(&user);
    let mut markup = views::sidebar(&context(&web, &viewer), &data, views::default_task(&data.tasks)).into_string();
    if data.images.iter().any(|i| i.digest.is_some()) {
        markup
            .push_str(&format!("<template data-images>{}</template>", views::images_card(&data.images).into_string()));
    }
    Ok(render(maud::PreEscaped(markup)))
}

async fn fragment_log<P: Platform>(
    State(web): State<Arc<Web<P>>>,
    _user: User,
    Path((build, attempt, task)): Path<(i64, i64, i64)>,
) -> WebResult<Response> {
    let store = &web.engine.store;
    let build = store.build(build)?.ok_or_else(|| WebError::NotFound("build not found".into()))?;
    let tasks = store.tasks(build.id, attempt)?;
    let task =
        tasks.into_iter().find(|t| t.ordinal == task).ok_or_else(|| WebError::NotFound("task not found".into()))?;
    let text = web.engine.logs.read(build.id, attempt, task.ordinal, 0);
    const SHOWN: usize = 1024 * 1024;
    let (skipped, shown) = if text.len() > SHOWN {
        let mut start = text.len() - SHOWN;
        while !text.is_char_boundary(start) {
            start += 1;
        }
        let start = text[start..].find('\n').map_or(start, |i| start + i + 1);
        let lines = text[..start].lines().filter(|line| kiln_runner::parse_marker(line).is_none()).count();
        (Some(lines), &text[start..])
    } else {
        (None, text.as_str())
    };
    Ok(render(views::log_view(&build, attempt, &task, shown, text.len(), skipped)))
}

async fn raw_log<P: Platform>(
    State(web): State<Arc<Web<P>>>,
    _user: User,
    Path((build, attempt, task)): Path<(i64, i64, String)>,
) -> WebResult<Response> {
    let task: i64 = task.trim_end_matches(".txt").parse().map_err(|_| WebError::NotFound("log not found".into()))?;
    let text = web.engine.logs.read(build, attempt, task, 0);
    let visible: String =
        text.lines().filter(|l| kiln_runner::parse_marker(l).is_none()).map(|l| format!("{l}\n")).collect();
    let name = format!("attachment; filename=\"kiln-build-{build}-attempt-{attempt}-task-{task}.log\"");
    Ok(([(CONTENT_TYPE, "text/plain; charset=utf-8".to_owned()), (CONTENT_DISPOSITION, name)], visible).into_response())
}

async fn trigger<P: Platform>(
    State(web): State<Arc<Web<P>>>,
    user: User,
    headers: HeaderMap,
    Path((owner, name)): Path<(String, String)>,
) -> WebResult<Response> {
    same_origin(&web, &headers)?;
    let repo = repo_named(&web, &owner, &name)?;
    match web.engine.trigger(repo.id, &user.login).await {
        Ok(id) => {
            let build = web.engine.store.build(id)?.ok_or_else(|| WebError::NotFound("build not found".into()))?;
            Ok(flash(&views::build_path(&repo, &build), "Build started"))
        }
        Err(error) => Ok(flash(&views::repo_path(&repo), &format!("Could not start a build: {error:#}"))),
    }
}

async fn pause<P: Platform>(
    State(web): State<Arc<Web<P>>>,
    user: User,
    headers: HeaderMap,
    Path((owner, name)): Path<(String, String)>,
) -> WebResult<Response> {
    same_origin(&web, &headers)?;
    let repo = repo_named(&web, &owner, &name)?;
    web.engine.set_paused(repo.id, true, &user.login)?;
    Ok(flash(&views::repo_path(&repo), "Paused"))
}

async fn resume<P: Platform>(
    State(web): State<Arc<Web<P>>>,
    user: User,
    headers: HeaderMap,
    Path((owner, name)): Path<(String, String)>,
) -> WebResult<Response> {
    same_origin(&web, &headers)?;
    let repo = repo_named(&web, &owner, &name)?;
    web.engine.set_paused(repo.id, false, &user.login)?;
    Ok(flash(&views::repo_path(&repo), "Resumed"))
}

async fn reset_cache<P: Platform>(
    State(web): State<Arc<Web<P>>>,
    user: User,
    headers: HeaderMap,
    Path((owner, name)): Path<(String, String)>,
) -> WebResult<Response> {
    same_origin(&web, &headers)?;
    let repo = repo_named(&web, &owner, &name)?;
    let message = match web.engine.reset_cache(repo.id, &user.login).await {
        Ok(()) => "Cache reset".to_owned(),
        Err(error) => format!("Could not reset the cache: {error:#}"),
    };
    Ok(flash(&views::repo_path(&repo), &message))
}

async fn dismiss_drift<P: Platform>(
    State(web): State<Arc<Web<P>>>,
    user: User,
    headers: HeaderMap,
    Path((owner, name)): Path<(String, String)>,
) -> WebResult<Response> {
    same_origin(&web, &headers)?;
    let repo = repo_named(&web, &owner, &name)?;
    web.engine.dismiss_drift(repo.id, &user.login)?;
    Ok(flash(&views::repo_path(&repo), "Dismissed"))
}

async fn cancel<P: Platform>(
    State(web): State<Arc<Web<P>>>,
    user: User,
    headers: HeaderMap,
    Path((owner, name, number)): Path<(String, String, i64)>,
) -> WebResult<Response> {
    same_origin(&web, &headers)?;
    let repo = repo_named(&web, &owner, &name)?;
    let build = web
        .engine
        .store
        .build_by_number(repo.id, number)?
        .ok_or_else(|| WebError::NotFound("build not found".into()))?;
    let message = match web.engine.cancel(build.id, &user.login) {
        Ok(()) => "Cancelling".to_owned(),
        Err(error) => format!("{error:#}"),
    };
    Ok(flash(&views::build_path(&repo, &build), &message))
}

async fn rerun<P: Platform>(
    State(web): State<Arc<Web<P>>>,
    user: User,
    headers: HeaderMap,
    Path((owner, name, number)): Path<(String, String, i64)>,
) -> WebResult<Response> {
    same_origin(&web, &headers)?;
    let repo = repo_named(&web, &owner, &name)?;
    let build = web
        .engine
        .store
        .build_by_number(repo.id, number)?
        .ok_or_else(|| WebError::NotFound("build not found".into()))?;
    match web.engine.rerun(build.id, &user.login).await {
        Ok(id) => {
            let rerun = web.engine.store.build(id)?.ok_or_else(|| WebError::NotFound("build not found".into()))?;
            Ok(flash(&views::build_path(&repo, &rerun), &format!("Rerunning #{}", build.number)))
        }
        Err(error) => Ok(flash(&views::build_path(&repo, &build), &format!("{error:#}"))),
    }
}

async fn refresh<P: Platform>(State(web): State<Arc<Web<P>>>, _user: User, headers: HeaderMap) -> WebResult<Response> {
    same_origin(&web, &headers)?;
    web.engine.poke(None);
    let back = headers
        .get(axum::http::header::REFERER)
        .and_then(|v| v.to_str().ok())
        .and_then(|r| r.strip_prefix(web.engine.settings.public_url.as_str()))
        .filter(|p| safe_next(p))
        .unwrap_or("/")
        .split('?')
        .next()
        .unwrap_or("/")
        .to_owned();
    Ok(flash(&back, "Checking GitHub for changes"))
}

async fn readyz<P: Platform>(State(web): State<Arc<Web<P>>>) -> (StatusCode, &'static str) {
    let engine = web.engine.clone();
    match tokio::task::spawn_blocking(move || engine.store.ping())
        .await
        .map_err(anyhow::Error::from)
        .and_then(|r| r.map_err(anyhow::Error::from))
    {
        Ok(()) => (StatusCode::OK, "ok"),
        Err(error) => {
            tracing::error!(%error, "the database is not reachable");
            (StatusCode::SERVICE_UNAVAILABLE, "database unavailable")
        }
    }
}

async fn settings_page<P: Platform>(State(web): State<Arc<Web<P>>>, user: User) -> WebResult<Response> {
    let viewer = viewer(&user);
    let store = &web.engine.store;
    let data = views::SettingsData {
        github: store.github()?,
        people: store.people()?,
        registry: web.engine.settings.registry.clone(),
        registry_username: web.engine.settings.registry_username.clone(),
        has_token: web.engine.settings.registry_password.is_some(),
        expires: web.engine.settings.registry_expires,
        secrets_error: web.engine.platform.registry_secrets_error(),
        you: user.login.clone(),
    };
    Ok(render(views::settings(&context(&web, &viewer), &data)))
}

#[derive(Deserialize)]
struct RegistryForm {
    username: Option<String>,
    token: String,
}

async fn save_registry<P: Platform>(
    State(web): State<Arc<Web<P>>>,
    user: User,
    headers: HeaderMap,
    axum::Form(form): axum::Form<RegistryForm>,
) -> WebResult<Response> {
    same_origin(&web, &headers)?;
    if web.demo {
        return Ok(flash("/settings", "Not available in the demo"));
    }
    let token = form.token.trim();
    let settings = &web.engine.settings;
    let (username, expires) = if settings.registry.starts_with("ghcr.io/") {
        let owner = match kiln_github::token_owner(&settings.github_api, token).await {
            Ok(owner) => owner,
            Err(kiln_github::Error::Status { status, .. }) if status == StatusCode::UNAUTHORIZED => {
                return Ok(flash("/settings", "GitHub did not accept this token"));
            }
            Err(error) => {
                tracing::warn!(%error, "checking the registry token failed");
                return Ok(flash("/settings", "GitHub did not answer; try again"));
            }
        };
        if !owner.scopes.iter().any(|s| s == "write:packages") {
            return Ok(flash("/settings", "Use a classic token with write:packages"));
        }
        (owner.login, owner.expires.as_deref().and_then(github_time))
    } else {
        let username = form.username.as_deref().unwrap_or_default().trim().to_owned();
        if username.is_empty() || token.is_empty() {
            return Ok(flash("/settings", "Enter a username and a token"));
        }
        (username, None)
    };
    web.engine.store.set_registry(&username, token, expires)?;
    web.engine.audit(&user.login, "registry_token_saved", None, None, Some(username.clone()));
    restart_soon();
    Ok(flash("/settings", "Saved; Kiln restarts to use it"))
}

async fn disconnect<P: Platform>(
    State(web): State<Arc<Web<P>>>,
    user: User,
    headers: HeaderMap,
) -> WebResult<Response> {
    same_origin(&web, &headers)?;
    if web.demo {
        return Ok(flash("/settings", "Not available in the demo"));
    }
    web.engine.store.disconnect_github()?;
    web.engine.audit(&user.login, "github_disconnected", None, None, None);
    restart_soon();
    Ok(flash("/", "Disconnected; Kiln restarts"))
}

fn github_time(text: &str) -> Option<i64> {
    let mut parts = text.split(|c: char| !c.is_ascii_digit()).filter(|p| !p.is_empty());
    let mut next = || parts.next()?.parse::<i32>().ok();
    let (year, month, day) = (next()?, next()?, next()?);
    let (hour, minute, second) = (next()?, next()?, next()?);
    let date = time::Date::from_calendar_date(
        year,
        time::Month::try_from(u8::try_from(month).ok()?).ok()?,
        u8::try_from(day).ok()?,
    )
    .ok()?;
    let time =
        time::Time::from_hms(u8::try_from(hour).ok()?, u8::try_from(minute).ok()?, u8::try_from(second).ok()?).ok()?;
    Some(date.with_time(time).assume_utc().unix_timestamp())
}

fn restart_soon() {
    tokio::spawn(async {
        tokio::time::sleep(Duration::from_millis(500)).await;
        super::RESTART.notify_one();
        tokio::time::sleep(Duration::from_secs(5)).await;
        std::process::exit(0);
    });
}

#[derive(Deserialize)]
struct PersonForm {
    login: String,
}

async fn add_person<P: Platform>(
    State(web): State<Arc<Web<P>>>,
    user: User,
    headers: HeaderMap,
    axum::Form(form): axum::Form<PersonForm>,
) -> WebResult<Response> {
    same_origin(&web, &headers)?;
    let login = form.login.trim().trim_start_matches('@');
    let found = match kiln_github::find_user(&web.engine.settings.github_api, login).await {
        Ok(found) => found,
        Err(error) if error.is_not_found() => return Ok(flash("/settings", &format!("No GitHub user named {login}"))),
        Err(error) => {
            tracing::warn!(%error, "looking up a GitHub user failed");
            return Ok(flash("/settings", "GitHub did not answer; try again in a minute"));
        }
    };
    web.engine.store.add_person(&kiln_store::Person {
        user_id: found.id as i64,
        login: found.login.clone(),
        added_by: user.login.clone(),
        added_at: unix_now(),
    })?;
    web.engine.audit(&user.login, "person_added", None, None, Some(found.login.clone()));
    Ok(flash("/settings", &format!("{} can sign in", found.login)))
}

async fn remove_person<P: Platform>(
    State(web): State<Arc<Web<P>>>,
    user: User,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> WebResult<Response> {
    same_origin(&web, &headers)?;
    let login = web.engine.store.people()?.into_iter().find(|p| p.user_id == id).map(|p| p.login).unwrap_or_default();
    if !web.engine.store.remove_person(id)? {
        return Ok(flash("/settings", "Someone has to keep access"));
    }
    web.engine.audit(&user.login, "person_removed", None, None, Some(login.clone()));
    Ok(flash("/settings", &format!("{login} removed")))
}

async fn system<P: Platform>(State(web): State<Arc<Web<P>>>, user: User) -> WebResult<Response> {
    let viewer = viewer(&user);
    let settings = &web.engine.settings;
    let mut facts = vec![
        ("Build slots", settings.slots.to_string()),
        ("Owners", settings.owners.join(", ")),
        ("Registry", settings.registry.clone()),
        ("Build namespace", settings.runner.namespace.clone()),
        ("Checks GitHub every", format!("{}s", settings.poll_secs)),
        ("Build timeout", super::checks::duration(settings.build_timeout_secs as i64)),
        ("Attempts", settings.max_attempts.to_string()),
        ("Sandbox runtime", settings.runner.sandbox_runtime_class.clone().unwrap_or_else(|| "default".into())),
    ];
    facts.extend(web.engine.platform.status());
    let store = &web.engine.store;
    let mut audit = Vec::new();
    for entry in store.audit_log(100)? {
        let target = match entry.build_id {
            Some(id) => store.build(id)?.and_then(|build| {
                store
                    .repo(build.repo_id)
                    .ok()
                    .flatten()
                    .map(|repo| (views::build_path(&repo, &build), format!("{} #{}", repo.name, build.number)))
            }),
            None => match entry.repo_id {
                Some(id) => store.repo(id)?.map(|repo| (views::repo_path(&repo), repo.name.clone())),
                None => None,
            },
        };
        let detail =
            entry.detail.clone().filter(|d| target.as_ref().is_none_or(|(_, label)| !d.ends_with(label.as_str())));
        audit.push((kiln_store::AuditEntry { detail, ..entry }, target));
    }
    let repos = store.repos()?;
    let data = views::SystemData {
        facts,
        audit,
        active: store.active()?.len(),
        slots: settings.slots,
        enrolled: repos.iter().filter(|r| r.enrolled && r.removed_at.is_none()).count(),
        visible: repos.iter().filter(|r| r.removed_at.is_none()).count(),
    };
    Ok(render(views::system(&context(&web, &viewer), &data)))
}

async fn events<P: Platform>(
    State(web): State<Arc<Web<P>>>,
    user: User,
) -> Sse<impl Stream<Item = Result<SseEvent, Infallible>>> {
    let receiver = web.engine.logs.events().subscribe();
    let store = web.engine.store.clone();
    let demo = web.demo;
    let revoked = async move {
        loop {
            tokio::time::sleep(Duration::from_secs(15)).await;
            let session = store.session(&user.session).ok().flatten();
            let allowed = demo || store.is_person(user.id).unwrap_or(false);
            if session.is_none() || !allowed {
                return;
            }
        }
    };
    let stream = tokio_stream::wrappers::BroadcastStream::new(receiver)
        .filter_map(|event| async move {
            let event = event.unwrap_or(crate::server::logs::Event::Resync);
            Some(Ok(SseEvent::default().data(serde_json::to_string(&event).ok()?)))
        })
        .take_until(Box::pin(revoked));
    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(20)))
}

#[derive(Deserialize, Default)]
struct LoginQuery {
    next: Option<String>,
    error: Option<String>,
}

async fn login<P: Platform>(
    State(web): State<Arc<Web<P>>>,
    Query(query): Query<LoginQuery>,
    jar: CookieJar,
) -> Response {
    let mut jar = jar;
    if let Some(next) = query.next.filter(|n| safe_next(n)) {
        jar = jar.add(cookie("kiln_next", next, 600));
    }
    let error = query.error.as_deref().map(|code| match code {
        "expired" => "Sign-in expired. Try again.",
        "verify" => "Sign-in failed. Try again.",
        "denied" => "This account has no access.",
        "config" => "GitHub sign-in is not configured.",
        _ => "Sign-in failed. Try again.",
    });
    (jar, render(views::login(error, web.demo))).into_response()
}

fn cookie(name: &str, value: String, max_age: i64) -> Cookie<'static> {
    let mut cookie = Cookie::new(name.to_owned(), value);
    cookie.set_path("/");
    cookie.set_secure(true);
    cookie.set_http_only(true);
    cookie.set_same_site(SameSite::Lax);
    cookie.set_max_age(time::Duration::seconds(max_age));
    cookie
}

async fn auth_start<P: Platform>(State(web): State<Arc<Web<P>>>, jar: CookieJar) -> WebResult<Response> {
    if web.demo {
        return start_session(&web, jar, 1, "demo", "", None).await;
    }
    let oauth =
        web.oauth.as_ref().ok_or_else(|| WebError::Internal(anyhow::anyhow!("GitHub sign-in is not configured")))?;
    let request = oauth.authorize();
    let jar = jar.add(cookie(OAUTH_COOKIE, format!("{}.{}", request.state, request.verifier), 600));
    Ok((jar, Redirect::to(&request.url)).into_response())
}

#[derive(Deserialize)]
struct Callback {
    code: Option<String>,
    state: Option<String>,
    error_description: Option<String>,
}

async fn auth_callback<P: Platform>(
    State(web): State<Arc<Web<P>>>,
    Query(callback): Query<Callback>,
    jar: CookieJar,
) -> WebResult<Response> {
    let back = |code: &str| Redirect::to(&format!("/login?error={code}")).into_response();
    if let Some(error) = callback.error_description {
        tracing::info!(error, "GitHub refused the sign-in");
        return Ok(back("github"));
    }
    let Some(oauth) = web.oauth.as_ref() else { return Ok(back("config")) };
    let Some((state, verifier)) =
        jar.get(OAUTH_COOKIE).and_then(|c| c.value().split_once('.').map(|(a, b)| (a.to_owned(), b.to_owned())))
    else {
        return Ok(back("expired"));
    };
    if callback.state.as_deref() != Some(state.as_str()) {
        return Ok(back("verify"));
    }
    let code = callback.code.ok_or_else(|| WebError::BadRequest("GitHub did not return a code".into()))?;
    let user = match oauth.user(&code, &verifier).await {
        Ok(user) => user,
        Err(error) => {
            tracing::warn!(%error, "GitHub sign-in failed");
            return Ok(back("github"));
        }
    };
    let jar = jar.remove(Cookie::build(OAUTH_COOKIE).path("/").secure(true));
    let store = &web.engine.store;
    let claimed = match jar.get(CLAIM_COOKIE) {
        Some(claim) => store.claim_first_person(
            claim.value(),
            &kiln_store::Person {
                user_id: user.id as i64,
                login: user.login.clone(),
                added_by: user.login.clone(),
                added_at: unix_now(),
            },
        )?,
        None => false,
    };
    let jar = jar.remove(Cookie::build(CLAIM_COOKIE).path("/").secure(true));
    if !claimed && !store.is_person(user.id as i64)? && web.claim_code.is_some() && store.people()?.is_empty() {
        let token = kiln_github::random_token();
        let mut claims = web.claims.lock().expect("claims");
        claims.retain(|_, (_, expires)| *expires > unix_now());
        claims.insert(token.clone(), (user, unix_now() + 900));
        return Ok((jar.add(cookie(PENDING_COOKIE, token, 900)), Redirect::to("/claim")).into_response());
    }
    if !claimed && !store.is_person(user.id as i64)? {
        web.engine.store.audit(&kiln_store::AuditEntry {
            at: unix_now(),
            actor: user.login.clone(),
            action: "login_denied".into(),
            repo_id: None,
            build_id: None,
            detail: Some(format!("GitHub user id {}", user.id)),
        })?;
        return Ok((jar, back("denied")).into_response());
    }
    start_session(&web, jar, user.id as i64, &user.login, &user.avatar_url, None).await
}

async fn claim_page<P: Platform>(State(web): State<Arc<Web<P>>>, jar: CookieJar) -> Response {
    let pending = jar.get(PENDING_COOKIE).is_some_and(|c| web.claims.lock().expect("claims").contains_key(c.value()));
    if !pending {
        return Redirect::to("/login").into_response();
    }
    render(views::claim(None))
}

#[derive(Deserialize)]
struct ClaimForm {
    code: String,
}

async fn claim<P: Platform>(
    State(web): State<Arc<Web<P>>>,
    headers: HeaderMap,
    jar: CookieJar,
    axum::Form(form): axum::Form<ClaimForm>,
) -> WebResult<Response> {
    same_origin(&web, &headers)?;
    let Some(token) = jar.get(PENDING_COOKIE).map(|c| c.value().to_owned()) else {
        return Ok(Redirect::to("/login").into_response());
    };
    let Some(expected) = web.claim_code.as_deref() else { return Ok(Redirect::to("/login").into_response()) };
    if !connect::same_code(&form.code, expected) {
        tokio::time::sleep(Duration::from_millis(800)).await;
        return Ok(render(views::claim(Some("That setup code is not right."))));
    }
    let Some((user, expires)) = web.claims.lock().expect("claims").remove(&token) else {
        return Ok(Redirect::to("/login").into_response());
    };
    if expires < unix_now() {
        return Ok(Redirect::to("/login?error=expired").into_response());
    }
    let person = kiln_store::Person {
        user_id: user.id as i64,
        login: user.login.clone(),
        added_by: user.login.clone(),
        added_at: unix_now(),
    };
    if !web.engine.store.add_first_person(&person)? {
        return Ok(Redirect::to("/login?error=denied").into_response());
    }
    web.engine.audit(&user.login, "kiln_claimed", None, None, None);
    let jar = jar.remove(Cookie::build(PENDING_COOKIE).path("/").secure(true));
    start_session(&web, jar, user.id as i64, &user.login, &user.avatar_url, None).await
}

async fn start_session<P: Platform>(
    web: &Web<P>,
    jar: CookieJar,
    id: i64,
    login: &str,
    avatar: &str,
    ip: Option<String>,
) -> WebResult<Response> {
    let token = kiln_github::random_token();
    let now = unix_now();
    web.engine.store.create_session(&Session {
        id_hash: hash(&token),
        user_id: id,
        login: login.to_owned(),
        avatar_url: avatar.to_owned(),
        created_at: now,
        seen_at: now,
        ip,
        agent: None,
    })?;
    web.engine.store.audit(&kiln_store::AuditEntry {
        at: now,
        actor: login.to_owned(),
        action: "login".into(),
        repo_id: None,
        build_id: None,
        detail: None,
    })?;
    let next =
        jar.get("kiln_next").map(|c| c.value().to_owned()).filter(|n| safe_next(n)).unwrap_or_else(|| "/".into());
    let jar =
        jar.add(cookie(SESSION_COOKIE, token, SESSION_MAX)).remove(Cookie::build("kiln_next").path("/").secure(true));
    Ok((jar, Redirect::to(&next)).into_response())
}

async fn logout<P: Platform>(
    State(web): State<Arc<Web<P>>>,
    user: User,
    headers: HeaderMap,
    jar: CookieJar,
) -> WebResult<Response> {
    same_origin(&web, &headers)?;
    web.engine.store.delete_session(&user.session)?;
    let jar = jar.remove(Cookie::build(SESSION_COOKIE).path("/").secure(true));
    Ok((jar, Redirect::to("/login")).into_response())
}

fn verify_signature(secrets: &[String], body: &[u8], header: Option<&str>) -> bool {
    let Some(signature) = header.and_then(|h| h.strip_prefix("sha256=")).and_then(|h| hex::decode(h).ok()) else {
        return false;
    };
    secrets.iter().any(|secret| {
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("any key length works");
        mac.update(body);
        mac.verify_slice(&signature).is_ok()
    })
}

async fn webhook<P: Platform>(State(web): State<Arc<Web<P>>>, headers: HeaderMap, body: Bytes) -> StatusCode {
    let signature = headers.get("x-hub-signature-256").and_then(|v| v.to_str().ok());
    if !verify_signature(&web.engine.settings.webhook_secrets, &body, signature) {
        tracing::warn!("rejected a webhook with a bad signature");
        return StatusCode::UNAUTHORIZED;
    }
    if let Some(delivery) = headers.get("x-github-delivery").and_then(|v| v.to_str().ok()) {
        let mut seen = web.deliveries.lock().expect("deliveries");
        if seen.iter().any(|d| d == delivery) {
            return StatusCode::ACCEPTED;
        }
        seen.push_back(delivery.to_owned());
        if seen.len() > 1000 {
            seen.pop_front();
        }
    }
    let event = headers.get("x-github-event").and_then(|v| v.to_str().ok()).unwrap_or_default();
    let Ok(payload) = serde_json::from_slice::<serde_json::Value>(&body) else { return StatusCode::BAD_REQUEST };
    let repo = payload["repository"]["id"].as_i64();
    match event {
        "ping" => {}
        "push" | "pull_request" | "create" | "delete" => web.engine.poke(repo),
        "installation" | "installation_repositories" | "repository" => web.engine.poke(None),
        "check_run" if payload["action"] == "rerequested" => {
            let sender = payload["sender"]["id"].as_u64().unwrap_or_default();
            let build = payload["check_run"]["external_id"].as_str().and_then(|id| id.parse::<i64>().ok());
            let login = payload["sender"]["login"].as_str().unwrap_or("github");
            if let Some(build) = build
                && web.engine.store.is_person(sender as i64).unwrap_or(false)
                && let Err(error) = web.engine.rerun(build, login).await
            {
                tracing::warn!(error = format!("{error:#}"), build, "rerun from GitHub failed");
            }
        }
        _ => {}
    }
    StatusCode::ACCEPTED
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_local_paths_are_followed_after_sign_in() {
        assert!(safe_next("/o/repo/builds/3?attempt=2"));
        for hostile in ["//evil.com", "/\\evil.com", "/\tevil.com", "/%09", "https://evil.com", "/ evil", "evil.com"] {
            assert_eq!(safe_next(hostile), hostile == "/%09", "{hostile:?}");
        }
    }

    #[test]
    fn webhooks_need_a_valid_signature_from_a_current_or_previous_secret() {
        let body = br#"{"zen":"Keep it logically awesome."}"#;
        let sign = |secret: &str| {
            let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
            mac.update(body);
            format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
        };
        let secrets = vec!["current".to_owned(), "previous".to_owned()];
        assert!(verify_signature(&secrets, body, Some(&sign("current"))));
        assert!(verify_signature(&secrets, body, Some(&sign("previous"))));
        assert!(!verify_signature(&secrets, body, Some(&sign("attacker"))));
        assert!(!verify_signature(&secrets, b"tampered", Some(&sign("current"))));
        assert!(!verify_signature(&secrets, body, None));
    }
}
