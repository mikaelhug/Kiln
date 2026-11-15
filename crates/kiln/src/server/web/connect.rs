use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::extract::{Query, State};
use axum::http::header::{CACHE_CONTROL, ORIGIN};
use axum::http::{HeaderMap, HeaderValue};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum_extra::extract::cookie::CookieJar;
use kiln_github::{convert_manifest, random_token, unix_now};
use kiln_store::{GitHubApp, Store};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::sync::Notify;
use tower_http::set_header::SetResponseHeaderLayer;

use super::{CLAIM_COOKIE, asset, cookie, views};

const SETUP_COOKIE: &str = "__Host-kiln_setup";

pub struct Connect {
    pub store: Arc<Store>,
    pub public_url: String,
    pub github_web: String,
    pub github_api: String,
    pub code: String,
    pub connected: Notify,
    pending: Mutex<Option<(String, i64)>>,
}

impl Connect {
    pub fn new(store: Arc<Store>, public_url: String, github_web: String, github_api: String) -> Self {
        Self {
            store,
            public_url,
            github_web,
            github_api,
            code: setup_code(),
            connected: Notify::new(),
            pending: Mutex::new(None),
        }
    }
}

pub fn setup_code() -> String {
    let letters: Vec<char> = random_token()
        .chars()
        .chain(random_token().chars())
        .map(|c| c.to_ascii_uppercase())
        .filter(|c| c.is_ascii_alphanumeric() && !"0O1I".contains(*c))
        .take(12)
        .collect();
    letters.chunks(4).map(|chunk| chunk.iter().collect::<String>()).collect::<Vec<_>>().join("-")
}

fn normalize(code: &str) -> String {
    code.chars().filter(|c| c.is_ascii_alphanumeric()).map(|c| c.to_ascii_uppercase()).collect()
}

pub fn same_code(given: &str, expected: &str) -> bool {
    Sha256::digest(normalize(given).as_bytes()) == Sha256::digest(normalize(expected).as_bytes())
}

pub fn router(connect: Arc<Connect>) -> Router {
    let policy = format!(
        "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self' {}",
        connect.github_web
    );
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/readyz", get(|| async { "ok" }))
        .route("/assets/kiln.css", get(|| async { asset("text/css; charset=utf-8", include_str!("assets/kiln.css")) }))
        .route(
            "/assets/connect.js",
            get(|| async { asset("text/javascript; charset=utf-8", include_str!("assets/connect.js")) }),
        )
        .route("/assets/favicon.svg", get(|| async { asset("image/svg+xml", include_str!("assets/favicon.svg")) }))
        .route("/setup", post(start))
        .route("/setup/callback", get(callback))
        .fallback(get(|| async { page(views::connect(None)) }))
        .layer(SetResponseHeaderLayer::overriding(
            axum::http::header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_str(&policy).unwrap_or_else(|_| HeaderValue::from_static("default-src 'self'")),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            axum::http::header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .with_state(connect)
}

fn page(markup: maud::Markup) -> Response {
    ([(CACHE_CONTROL, "no-store")], markup).into_response()
}

fn manifest(url: &str, redirect: &str) -> serde_json::Value {
    let host = url.trim_start_matches("https://").trim_start_matches("http://");
    serde_json::json!({
        "name": format!("Kiln {host}").chars().take(34).collect::<String>(),
        "url": url,
        "description": "Self-hosted CI: tests, image builds and releases.",
        "hook_attributes": { "url": format!("{url}/hooks/github"), "active": true },
        "redirect_url": redirect,
        "callback_urls": [format!("{url}/auth/callback")],
        "public": false,
        "request_oauth_on_install": false,
        "default_permissions": {
            "contents": "read",
            "checks": "write",
            "pull_requests": "read",
            "metadata": "read"
        },
        "default_events": ["push", "pull_request", "check_run", "create", "repository"]
    })
}

#[derive(Deserialize)]
struct Start {
    code: String,
    org: Option<String>,
}

async fn start(
    State(connect): State<Arc<Connect>>,
    headers: HeaderMap,
    jar: CookieJar,
    axum::Form(form): axum::Form<Start>,
) -> Response {
    let origin = headers.get(ORIGIN).and_then(|v| v.to_str().ok()).unwrap_or_default();
    if origin != connect.public_url {
        return page(views::connect(Some("Open Kiln at its own address and try again.")));
    }
    if !same_code(&form.code, &connect.code) {
        tokio::time::sleep(Duration::from_millis(800)).await;
        return page(views::connect(Some("That setup code is not right.")));
    }
    let org = form.org.map(|o| o.trim().to_owned()).filter(|o| !o.is_empty());
    if org.as_deref().is_some_and(|o| !o.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')) {
        return page(views::connect(Some("That organization name is not valid.")));
    }
    let state = random_token();
    *connect.pending.lock().expect("pending setup") = Some((state.clone(), unix_now() + 900));
    let github = connect.github_web.trim_end_matches('/');
    let action = match &org {
        Some(org) => format!("{github}/organizations/{org}/settings/apps/new?state={state}"),
        None => format!("{github}/settings/apps/new?state={state}"),
    };
    let redirect = format!("{}/setup/callback", connect.public_url);
    let manifest = manifest(&connect.public_url, &redirect).to_string();
    (jar.add(cookie(SETUP_COOKIE, state, 900)), page(views::connect_redirect(&action, &manifest))).into_response()
}

#[derive(Deserialize)]
struct Callback {
    code: Option<String>,
    state: Option<String>,
}

async fn callback(State(connect): State<Arc<Connect>>, jar: CookieJar, Query(query): Query<Callback>) -> Response {
    let expected = connect.pending.lock().expect("pending setup").clone();
    let valid = matches!(
        (&expected, &query.state, jar.get(SETUP_COOKIE)),
        (Some((state, expires)), Some(given), Some(cookie))
            if state == given && cookie.value() == state && *expires > unix_now()
    );
    let Some(code) = query.code.filter(|_| valid) else {
        return page(views::connect(Some("This link is not from this setup. Start again.")));
    };
    let created = match convert_manifest(&connect.github_api, &code).await {
        Ok(created) => created,
        Err(error) => {
            tracing::warn!(%error, "converting the GitHub App manifest failed");
            return page(views::connect(Some("GitHub did not hand over the app. Start again.")));
        }
    };
    let Some(webhook_secret) = created.webhook_secret.clone() else {
        return page(views::connect(Some("GitHub did not return a webhook secret. Start again.")));
    };
    let owner = created.owner.as_ref();
    let claim = random_token();
    let app = GitHubApp {
        app_id: created.id as i64,
        slug: created.slug.clone(),
        name: created.name.clone(),
        html_url: created.html_url.clone(),
        client_id: created.client_id.clone(),
        client_secret: created.client_secret.clone(),
        webhook_secret,
        private_key: created.pem.clone(),
        owner_login: owner.map(|o| o.login.clone()).unwrap_or_default(),
        owner_id: owner.map(|o| o.id as i64).unwrap_or_default(),
        owner_is_user: owner.is_some_and(|o| o.kind == "User"),
        claim_token: Some(claim.clone()),
        registry_username: None,
        registry_token: None,
        connected_at: unix_now(),
        registry_expires: None,
    };
    match connect.store.connect_github(&app) {
        Ok(true) => *connect.pending.lock().expect("pending setup") = None,
        Ok(false) => return page(views::connect(Some("Kiln is already connected."))),
        Err(error) => {
            tracing::error!(%error, "storing the GitHub App failed");
            return page(views::connect(Some("Kiln could not save the app. Check its log.")));
        }
    }
    tracing::info!(app = app.slug, owner = app.owner_login, "connected to GitHub");
    let connect_done = connect.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        connect_done.connected.notify_one();
    });
    let install = format!("{}/installations/new", app.html_url.trim_end_matches('/'));
    let jar = jar
        .add(cookie(CLAIM_COOKIE, claim, 86_400))
        .remove(axum_extra::extract::cookie::Cookie::build(SETUP_COOKIE).path("/").secure(true));
    (jar, page(views::connected(&app.name, &install))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_codes_are_readable_and_compared_loosely() {
        let code = setup_code();
        assert_eq!(code.len(), 14, "{code}");
        assert!(same_code(&code.to_lowercase().replace('-', " "), &code));
        assert!(!same_code("AAAA-BBBB-CCCC", &code));
    }
}
