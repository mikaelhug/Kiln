mod app;
mod checks;
mod oauth;
mod repos;

use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::header::{ACCEPT, AUTHORIZATION, DATE, ETAG, HeaderMap, IF_NONE_MATCH, USER_AGENT};
use reqwest::{Method, RequestBuilder, Response, StatusCode};
use serde::de::DeserializeOwned;
use thiserror::Error;
use tokio::sync::Mutex;

pub use app::{AppKey, CreatedApp, ManifestOwner, convert_manifest};
pub use checks::{Annotation, CheckConclusion, CheckOutput, CheckRun, CheckStatus};
pub use oauth::{AuthorizeRequest, GitHubUser, OAuthApp, find_user, random_token};
pub use repos::{Account, Compare, Head, Installation, PullRequest, Repository, TagRef, Token, download_tarball};

const API_VERSION: &str = "2022-11-28";
const AGENT: &str = concat!("kiln/", env!("CARGO_PKG_VERSION"));

#[derive(Debug, Error)]
pub enum Error {
    #[error("GitHub answered {status}: {message}")]
    Status { status: StatusCode, message: String },
    #[error("GitHub rate limit reached, resets at unix time {reset}")]
    RateLimited { reset: u64 },
    #[error("request to GitHub failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("signing the app token failed: {0}")]
    Jwt(#[from] jsonwebtoken::errors::Error),
    #[error("GitHub returned an unexpected response: {0}")]
    Shape(String),
}

impl Error {
    pub fn is_not_found(&self) -> bool {
        matches!(self, Error::Status { status: StatusCode::NOT_FOUND, .. })
    }

    pub fn is_transient(&self) -> bool {
        match self {
            Error::Status { status, .. } => status.is_server_error() || *status == StatusCode::TOO_MANY_REQUESTS,
            Error::RateLimited { .. } | Error::Http(_) => true,
            Error::Jwt(_) | Error::Shape(_) => false,
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Conditional<T> {
    NotModified,
    Fresh { value: T, etag: Option<String> },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RateLimit {
    pub remaining: Option<u64>,
    pub reset: Option<u64>,
}

pub struct Client {
    http: reqwest::Client,
    api: String,
    app: AppKey,
    clock_offset: AtomicI64,
    installation_tokens: Mutex<Vec<(u64, Token)>>,
    rate: std::sync::Mutex<RateLimit>,
}

impl Client {
    pub fn new(api: &str, app: AppKey) -> Result<Self> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(120))
            .build()?;
        Ok(Self {
            http,
            api: api.trim_end_matches('/').to_owned(),
            app,
            clock_offset: AtomicI64::new(0),
            installation_tokens: Mutex::new(Vec::new()),
            rate: std::sync::Mutex::new(RateLimit::default()),
        })
    }

    pub fn rate_limit(&self) -> RateLimit {
        *self.rate.lock().expect("rate limit lock")
    }

    pub fn now(&self) -> i64 {
        unix_now() + self.clock_offset.load(Ordering::Relaxed)
    }

    fn request(&self, method: Method, path: &str, auth: &str) -> RequestBuilder {
        let url = if path.starts_with("https://") || path.starts_with("http://") {
            path.to_owned()
        } else {
            format!("{}{path}", self.api)
        };
        self.http
            .request(method, url)
            .header(USER_AGENT, AGENT)
            .header(ACCEPT, "application/vnd.github+json")
            .header("X-GitHub-Api-Version", API_VERSION)
            .header(AUTHORIZATION, auth)
    }

    async fn send(&self, request: RequestBuilder) -> Result<Response> {
        let response = request.send().await?;
        self.record_rate(response.headers());
        let status = response.status();
        if status.is_success() || status == StatusCode::NOT_MODIFIED {
            return Ok(response);
        }
        let reset = header_u64(response.headers(), "x-ratelimit-reset");
        let exhausted = header_u64(response.headers(), "x-ratelimit-remaining") == Some(0);
        if (status == StatusCode::FORBIDDEN || status == StatusCode::TOO_MANY_REQUESTS) && exhausted {
            return Err(Error::RateLimited { reset: reset.unwrap_or_default() });
        }
        let message = response
            .json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|body| body.get("message").and_then(|m| m.as_str()).map(str::to_owned))
            .unwrap_or_default();
        Err(Error::Status { status, message })
    }

    fn record_rate(&self, headers: &HeaderMap) {
        if let Some(remaining) = header_u64(headers, "x-ratelimit-remaining") {
            let mut rate = self.rate.lock().expect("rate limit lock");
            rate.remaining = Some(remaining);
            rate.reset = header_u64(headers, "x-ratelimit-reset");
        }
    }

    async fn as_app(&self, method: Method, path: &str, body: Option<&serde_json::Value>) -> Result<Response> {
        let build = |jwt: String| {
            let request = self.request(method.clone(), path, &format!("Bearer {jwt}"));
            match body {
                Some(body) => request.json(body),
                None => request,
            }
        };
        let response = build(self.app.sign(self.now())?).send().await?;
        if response.status() == StatusCode::UNAUTHORIZED
            && let Some(server) = response.headers().get(DATE).and_then(|v| v.to_str().ok()).and_then(parse_http_date)
        {
            self.clock_offset.store(server - unix_now(), Ordering::Relaxed);
            return self.send(build(self.app.sign(self.now())?)).await;
        }
        let status = response.status();
        if status.is_success() {
            self.record_rate(response.headers());
            return Ok(response);
        }
        let message = response.text().await.unwrap_or_default();
        Err(Error::Status { status, message })
    }

    async fn installation_auth(&self, installation: u64) -> Result<String> {
        let mut cache = self.installation_tokens.lock().await;
        let now = self.now();
        if let Some((_, token)) = cache.iter().find(|(id, token)| *id == installation && token.expires_at - 300 > now) {
            return Ok(format!("Bearer {}", token.token));
        }
        let token = self.mint_token(installation, &serde_json::json!({})).await?;
        let auth = format!("Bearer {}", token.token);
        cache.retain(|(id, _)| *id != installation);
        cache.push((installation, token));
        Ok(auth)
    }

    async fn installation_json<T: DeserializeOwned>(
        &self,
        installation: u64,
        method: Method,
        path: &str,
        body: Option<&serde_json::Value>,
    ) -> Result<T> {
        let auth = self.installation_auth(installation).await?;
        let mut request = self.request(method, path, &auth);
        if let Some(body) = body {
            request = request.json(body);
        }
        Ok(self.send(request).await?.json().await?)
    }

    async fn get_conditional<T: DeserializeOwned>(
        &self,
        installation: u64,
        path: &str,
        etag: Option<&str>,
    ) -> Result<Conditional<T>> {
        let auth = self.installation_auth(installation).await?;
        let mut request = self.request(Method::GET, path, &auth);
        if let Some(etag) = etag {
            request = request.header(IF_NONE_MATCH, etag);
        }
        let response = self.send(request).await?;
        if response.status() == StatusCode::NOT_MODIFIED {
            return Ok(Conditional::NotModified);
        }
        let etag = response.headers().get(ETAG).and_then(|v| v.to_str().ok()).map(str::to_owned);
        Ok(Conditional::Fresh { value: response.json().await?, etag })
    }

    async fn get_conditional_list<T: DeserializeOwned>(
        &self,
        installation: u64,
        path: &str,
        etag: Option<&str>,
    ) -> Result<Conditional<Vec<T>>> {
        let auth = self.installation_auth(installation).await?;
        let mut request = self.request(Method::GET, path, &auth);
        if let Some(etag) = etag {
            request = request.header(IF_NONE_MATCH, etag);
        }
        let response = self.send(request).await?;
        if response.status() == StatusCode::NOT_MODIFIED {
            return Ok(Conditional::NotModified);
        }
        let next = next_link(response.headers());
        let etag = if next.is_some() {
            None
        } else {
            response.headers().get(ETAG).and_then(|v| v.to_str().ok()).map(str::to_owned)
        };
        let mut value: Vec<T> = response.json().await?;
        if let Some(next) = next {
            value.extend(
                self.paginate::<T>(&auth, &next, |body| match body {
                    serde_json::Value::Array(items) => items,
                    _ => Vec::new(),
                })
                .await?,
            );
        }
        Ok(Conditional::Fresh { value, etag })
    }

    async fn paginate<T: DeserializeOwned>(
        &self,
        auth: &str,
        first: &str,
        items: impl Fn(serde_json::Value) -> Vec<serde_json::Value>,
    ) -> Result<Vec<T>> {
        let mut out = Vec::new();
        let mut next = Some(first.to_owned());
        while let Some(path) = next.take() {
            let response = self.send(self.request(Method::GET, &path, auth)).await?;
            next = next_link(response.headers());
            let body: serde_json::Value = response.json().await?;
            for item in items(body) {
                out.push(serde_json::from_value(item).map_err(|e| Error::Shape(e.to_string()))?);
            }
        }
        Ok(out)
    }
}

pub fn unix_now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or_default()
}

fn header_u64(headers: &HeaderMap, name: &str) -> Option<u64> {
    headers.get(name)?.to_str().ok()?.parse().ok()
}

fn next_link(headers: &HeaderMap) -> Option<String> {
    let link = headers.get("link")?.to_str().ok()?;
    link.split(',').find_map(|part| {
        let (url, rel) = part.split_once(';')?;
        rel.contains("rel=\"next\"").then(|| url.trim().trim_start_matches('<').trim_end_matches('>').to_owned())
    })
}

fn parse_http_date(value: &str) -> Option<i64> {
    let parts: Vec<&str> = value.split_whitespace().collect();
    let [_, day, month, year, time, "GMT"] = parts.as_slice() else { return None };
    let month = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"]
        .iter()
        .position(|m| m == month)? as i64
        + 1;
    unix_from_civil(year.parse().ok()?, month, day.parse().ok()?, time)
}

pub fn parse_rfc3339(value: &str) -> Option<i64> {
    let value = value.strip_suffix('Z')?;
    let (date, time) = value.split_once('T')?;
    let mut parts = date.split('-').map(|p| p.parse::<i64>().ok());
    let (year, month, day) = (parts.next()??, parts.next()??, parts.next()??);
    unix_from_civil(year, month, day, time.split('.').next()?)
}

fn unix_from_civil(year: i64, month: i64, day: i64, time: &str) -> Option<i64> {
    let mut clock = time.split(':').map(|p| p.parse::<i64>().ok());
    let (h, m, s) = (clock.next()??, clock.next()??, clock.next()??);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let (y, mo) = if month <= 2 { (year - 1, month + 9) } else { (year, month - 3) };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * mo + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some((era * 146_097 + doe - 719_468) * 86_400 + h * 3600 + m * 60 + s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_dates_set_the_clock_offset() {
        assert_eq!(parse_http_date("Sat, 14 Nov 2026 09:22:42 GMT"), Some(1_794_648_162));
        assert_eq!(parse_http_date("Thu, 01 Jan 1970 00:00:00 GMT"), Some(0));
        assert_eq!(parse_http_date("Tue, 29 Feb 2028 23:59:59 GMT"), Some(1_835_481_599));
        assert_eq!(parse_http_date("garbage"), None);
        assert_eq!(parse_rfc3339("2026-11-14T09:22:42Z"), Some(1_794_648_162));
        assert_eq!(parse_rfc3339("2026-11-14T09:22:42.123Z"), Some(1_794_648_162));
    }
}
