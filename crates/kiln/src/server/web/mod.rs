mod ansi;
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
const SESSION_IDLE: i64 = 12 * 3600;
const SESSION_MAX: i64 = 7 * 86_400;
const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' https://avatars.githubusercontent.com; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'";

pub struct Web<P: Platform> {
    pub engine: Arc<Engine<P>>,
    pub oauth: Option<OAuthApp>,
    pub demo: bool,
}

pub fn router<P: Platform>(web: Arc<Web<P>>) -> Router {
    Router::new()
        .route("/", get(dashboard::<P>))
        .route("/repos", get(repos::<P>))
        .route("/system", get(system::<P>))
        .route("/system/refresh", post(refresh::<P>))
        .route("/events", get(events::<P>))
        .route("/fragments/dashboard", get(fragment_dashboard::<P>))
        .route("/fragments/repos", get(fragment_repos::<P>))
        .route("/fragments/repo/{id}", get(fragment_repo::<P>))
        .route("/fragments/build/{id}", get(fragment_build::<P>))
        .route("/fragments/tasks/{id}", get(fragment_tasks::<P>))
        .route("/fragments/log/{build}/{attempt}/{task}", get(fragment_log::<P>))
        .route("/builds/{build}/logs/{attempt}/{task}", get(raw_log::<P>))
        .route("/login", get(login::<P>))
        .route("/auth/github", get(auth_start::<P>))
        .route("/auth/callback", get(auth_callback::<P>))
        .route("/logout", post(logout::<P>))
        .route("/hooks/github", post(webhook::<P>))
        .route("/healthz", get(|| async { "ok" }))
        .route("/assets/kiln.css", get(|| async { asset("text/css; charset=utf-8", include_str!("assets/kiln.css")) }))
        .route(
            "/assets/kiln.js",
            get(|| async { asset("text/javascript; charset=utf-8", include_str!("assets/kiln.js")) }),
        )
        .route("/assets/favicon.svg", get(|| async { asset("image/svg+xml", include_str!("assets/favicon.svg")) }))
        .route("/{owner}/{repo}", get(repo_page::<P>))
        .route("/{owner}/{repo}/build", post(trigger::<P>))
        .route("/{owner}/{repo}/pause", post(pause::<P>))
        .route("/{owner}/{repo}/resume", post(resume::<P>))
        .route("/{owner}/{repo}/reset-cache", post(reset_cache::<P>))
        .route("/{owner}/{repo}/builds/{number}", get(build_page::<P>))
        .route("/{owner}/{repo}/builds/{number}/cancel", post(cancel::<P>))
        .route("/{owner}/{repo}/builds/{number}/rerun", post(rerun::<P>))
        .layer(DefaultBodyLimit::max(5 * 1024 * 1024))
        .layer(SetResponseHeaderLayer::overriding(
            axum::http::header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(CSP),
        ))
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

fn asset(kind: &'static str, body: &'static str) -> Response {
    ([(CONTENT_TYPE, kind), (CACHE_CONTROL, "public, max-age=31536000, immutable")], body).into_response()
}

pub enum WebError {
    SignIn(String),
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
            WebError::NotFound(message) => (StatusCode::NOT_FOUND, message),
            WebError::Forbidden(message) => (StatusCode::FORBIDDEN, message),
            WebError::BadRequest(message) => (StatusCode::BAD_REQUEST, message),
            WebError::Internal(error) => {
                tracing::error!(error = format!("{error:#}"), "request failed");
                (StatusCode::INTERNAL_SERVER_ERROR, "Something went wrong. The details are in Kiln's log.".to_owned())
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
        let allowed = web.demo || web.engine.settings.allowed_users.contains(&(session.user_id as u64));
        if !allowed || now - session.seen_at > SESSION_IDLE || now - session.created_at > SESSION_MAX {
            store.delete_session(&id_hash)?;
            return Err(sign_in(parts, next));
        }
        if now - session.seen_at > 60 {
            store.touch_session(&id_hash, now)?;
        }
        Ok(User { login: session.login, avatar_url: session.avatar_url, session: id_hash })
    }
}

fn sign_in(parts: &Parts, next: String) -> WebError {
    if parts.headers.contains_key("x-kiln-fragment") || parts.uri.path().starts_with("/events") {
        return WebError::Forbidden("sign in again".into());
    }
    WebError::SignIn(next)
}

fn same_origin<P: Platform>(web: &Web<P>, headers: &HeaderMap) -> WebResult<()> {
    let expected = web.engine.settings.public_url.trim_end_matches('/');
    let origin = headers.get(ORIGIN).and_then(|v| v.to_str().ok()).unwrap_or_default();
    if origin == expected {
        Ok(())
    } else {
        Err(WebError::Forbidden("This form was submitted from another site.".into()))
    }
}

fn context<'a, P: Platform>(web: &'a Web<P>, viewer: &'a Viewer) -> Context<'a> {
    Context { viewer, github_web: &web.engine.settings.github_web, demo: web.demo }
}

fn viewer(user: &User) -> Viewer {
    Viewer { login: user.login.clone(), avatar_url: user.avatar_url.clone() }
}

fn flash(path: &str, message: &str) -> Response {
    let separator = if path.contains('?') { '&' } else { '?' };
    Redirect::to(&format!("{path}{separator}flash={}", urlencode(message))).into_response()
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

fn latest<P: Platform>(
    web: &Web<P>,
    repos: &[Repo],
) -> WebResult<Vec<(Repo, Option<kiln_store::Build>, Option<String>)>> {
    let store = &web.engine.store;
    let mut out = Vec::new();
    for repo in repos {
        let builds = store.builds(&BuildFilter { repo: Some(repo.id), limit: 40, ..Default::default() })?;
        let main = builds.iter().find(|b| b.trigger != Trigger::PullRequest).cloned();
        let version = builds
            .iter()
            .find(|b| {
                b.state == BuildState::Succeeded && b.detail.as_deref().is_some_and(|d| d.starts_with("Released"))
            })
            .and_then(|b| b.version.clone());
        out.push((repo.clone(), main, version));
    }
    Ok(out)
}

fn dashboard_data<P: Platform>(web: &Web<P>) -> WebResult<views::DashboardData> {
    let store = &web.engine.store;
    let repos = store.repos()?;
    let mut active = store.active()?;
    active.reverse();
    let mut queued = store.queued()?;
    queued.reverse();
    let recent = store
        .builds(&BuildFilter { limit: 40, ..Default::default() })?
        .into_iter()
        .filter(|b| b.state.is_finished())
        .take(25)
        .collect();
    let visible: Vec<Repo> = repos.iter().filter(|r| r.removed_at.is_none()).cloned().collect();
    let latest = latest(web, &visible)?;
    Ok(views::DashboardData { repos, active, queued, recent, latest, slots: web.engine.settings.slots })
}

async fn dashboard<P: Platform>(State(web): State<Arc<Web<P>>>, user: User) -> WebResult<Response> {
    let viewer = viewer(&user);
    Ok(render(views::dashboard(&context(&web, &viewer), &dashboard_data(&web)?)))
}

async fn fragment_dashboard<P: Platform>(State(web): State<Arc<Web<P>>>, _user: User) -> WebResult<Response> {
    Ok(render(views::dashboard_live(&dashboard_data(&web)?)))
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
}

fn repo_data<P: Platform>(web: &Web<P>, repo: Repo, before: Option<i64>) -> WebResult<views::RepoData> {
    let store = &web.engine.store;
    let page = 30;
    let mut builds =
        store.builds(&BuildFilter { repo: Some(repo.id), before, limit: page + 1, ..Default::default() })?;
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
        .builds(&BuildFilter { repo: Some(repo.id), limit: 20, ..Default::default() })?
        .into_iter()
        .filter(|b| b.trigger != Trigger::PullRequest)
        .find_map(|b| b.config.and_then(|text| config::parse(&text, &repo.name).ok()));
    let busy = store.active()?.iter().any(|b| b.repo_id == repo.id);
    Ok(views::RepoData { repo, builds, releases, config, more, busy })
}

async fn repo_page<P: Platform>(
    State(web): State<Arc<Web<P>>>,
    user: User,
    Path((owner, name)): Path<(String, String)>,
    Query(paging): Query<Paging>,
) -> WebResult<Response> {
    let viewer = viewer(&user);
    let repo = repo_named(&web, &owner, &name)?;
    let data = repo_data(&web, repo, paging.before)?;
    Ok(render(views::repo(&context(&web, &viewer), &data)))
}

async fn fragment_repo<P: Platform>(
    State(web): State<Arc<Web<P>>>,
    user: User,
    Path(id): Path<i64>,
) -> WebResult<Response> {
    let viewer = viewer(&user);
    let repo = web.engine.store.repo(id)?.ok_or_else(|| WebError::NotFound("repository not found".into()))?;
    let data = repo_data(&web, repo, None)?;
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
    Ok(views::BuildData { repo, build, attempt, tasks, images, rerun_of })
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
    _user: User,
    Path(id): Path<i64>,
    Query(query): Query<AttemptQuery>,
) -> WebResult<Response> {
    let build = web.engine.store.build(id)?.ok_or_else(|| WebError::NotFound("build not found".into()))?;
    let data = build_data(&web, build, query.attempt)?;
    let mut markup = views::tasks_card(&data, views::default_task(&data.tasks)).into_string();
    if !data.images.is_empty() {
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
    Ok(render(views::log_view(&build, attempt, &task, &text)))
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
    Ok(flash(&views::repo_path(&repo), "Resumed; checking GitHub for new commits"))
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
        Ok(()) => "Cache deleted; the next build starts cold".to_owned(),
        Err(error) => format!("Could not reset the cache: {error:#}"),
    };
    Ok(flash(&views::repo_path(&repo), &message))
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
    match web.engine.rerun(build.id, &user.login) {
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
        .filter(|p| p.starts_with('/') && !p.starts_with("//"))
        .unwrap_or("/")
        .split('?')
        .next()
        .unwrap_or("/")
        .to_owned();
    Ok(flash(&back, "Checking GitHub for changes"))
}

async fn system<P: Platform>(State(web): State<Arc<Web<P>>>, user: User) -> WebResult<Response> {
    let viewer = viewer(&user);
    let settings = &web.engine.settings;
    let facts = vec![
        ("Build slots", settings.slots.to_string()),
        ("Owners", settings.owners.join(", ")),
        ("Registry", settings.registry.clone()),
        ("Build namespace", settings.runner.namespace.clone()),
        ("Checks GitHub every", format!("{}s", settings.poll_secs)),
        ("Build timeout", super::checks::duration(settings.build_timeout_secs as i64)),
        ("Attempts", settings.max_attempts.to_string()),
        ("User namespaces", if settings.runner.user_namespaces { "on".into() } else { "off".into() }),
    ];
    let audit = web.engine.store.audit_log(100)?;
    Ok(render(views::system(&context(&web, &viewer), &facts, &audit)))
}

async fn events<P: Platform>(
    State(web): State<Arc<Web<P>>>,
    _user: User,
) -> Sse<impl Stream<Item = Result<SseEvent, Infallible>>> {
    let receiver = web.engine.logs.events().subscribe();
    let stream = tokio_stream::wrappers::BroadcastStream::new(receiver).filter_map(|event| async move {
        let event = event.ok()?;
        Some(Ok(SseEvent::default().data(serde_json::to_string(&event).ok()?)))
    });
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
    if let Some(next) = query.next.filter(|n| n.starts_with('/') && !n.starts_with("//")) {
        jar = jar.add(cookie("kiln_next", next, 600));
    }
    (jar, render(views::login(query.error.as_deref(), web.demo))).into_response()
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
    let back = |message: &str| Redirect::to(&format!("/login?error={}", urlencode(message))).into_response();
    if let Some(error) = callback.error_description {
        return Ok(back(&error));
    }
    let Some(oauth) = web.oauth.as_ref() else { return Ok(back("GitHub sign-in is not configured")) };
    let Some((state, verifier)) =
        jar.get(OAUTH_COOKIE).and_then(|c| c.value().split_once('.').map(|(a, b)| (a.to_owned(), b.to_owned())))
    else {
        return Ok(back("The sign-in took too long. Please try again."));
    };
    if callback.state.as_deref() != Some(state.as_str()) {
        return Ok(back("The sign-in could not be verified. Please try again."));
    }
    let code = callback.code.ok_or_else(|| WebError::BadRequest("GitHub did not return a code".into()))?;
    let user = match oauth.user(&code, &verifier).await {
        Ok(user) => user,
        Err(error) => return Ok(back(&format!("GitHub sign-in failed: {error}"))),
    };
    let jar = jar.remove(Cookie::build(OAUTH_COOKIE).path("/"));
    if !web.engine.settings.allowed_users.contains(&user.id) {
        web.engine.store.audit(&kiln_store::AuditEntry {
            at: unix_now(),
            actor: user.login.clone(),
            action: "login_denied".into(),
            repo_id: None,
            build_id: None,
            detail: Some(format!("GitHub user id {}", user.id)),
        })?;
        return Ok((jar, back(&format!("@{} is not allowed to use this Kiln.", user.login))).into_response());
    }
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
    let next = jar
        .get("kiln_next")
        .map(|c| c.value().to_owned())
        .filter(|n| n.starts_with('/') && !n.starts_with("//"))
        .unwrap_or_else(|| "/".into());
    let jar = jar.add(cookie(SESSION_COOKIE, token, SESSION_MAX)).remove(Cookie::build("kiln_next").path("/"));
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
    let jar = jar.remove(Cookie::build(SESSION_COOKIE).path("/"));
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
                && web.engine.settings.allowed_users.contains(&sender)
                && let Err(error) = web.engine.rerun(build, login)
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
