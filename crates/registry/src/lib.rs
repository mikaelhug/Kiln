use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE, HeaderMap, LOCATION, WWW_AUTHENTICATE};
use reqwest::{Method, RequestBuilder, Response, StatusCode};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::sync::Mutex;

pub const OCI_INDEX: &str = "application/vnd.oci.image.index.v1+json";
pub const OCI_MANIFEST: &str = "application/vnd.oci.image.manifest.v1+json";
pub const DOCKER_LIST: &str = "application/vnd.docker.distribution.manifest.list.v2+json";
pub const DOCKER_MANIFEST: &str = "application/vnd.docker.distribution.manifest.v2+json";
const MANIFEST_TYPES: [&str; 4] = [OCI_INDEX, OCI_MANIFEST, DOCKER_LIST, DOCKER_MANIFEST];

#[derive(Debug, Error)]
pub enum Error {
    #[error("registry answered {status} for {what}: {message}")]
    Status { status: StatusCode, what: String, message: String },
    #[error("request to the registry failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("reading the image layout failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("the image layout is invalid: {0}")]
    Layout(String),
    #[error("digest mismatch for {what}: expected {expected}, got {actual}")]
    DigestMismatch { what: String, expected: String, actual: String },
    #[error("{repository}:{tag} already points at {existing}; semver tags are immutable, release a new version")]
    TagTaken { repository: String, tag: String, existing: String },
}

impl Error {
    pub fn is_denied(&self) -> bool {
        matches!(self, Error::Status { status, .. } if *status == StatusCode::UNAUTHORIZED || *status == StatusCode::FORBIDDEN)
    }

    pub fn is_transient(&self) -> bool {
        match self {
            Error::Status { status, .. } => status.is_server_error() || *status == StatusCode::TOO_MANY_REQUESTS,
            Error::Http(_) => true,
            _ => false,
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub media_type: String,
    pub digest: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Promotion {
    Created,
    AlreadyThere,
}

#[derive(Debug, Clone, Deserialize)]
struct Descriptor {
    #[serde(rename = "mediaType")]
    media_type: String,
    digest: String,
    size: u64,
}

#[derive(Debug, Deserialize)]
struct Links {
    #[serde(default)]
    config: Option<Descriptor>,
    #[serde(default)]
    layers: Vec<Descriptor>,
    #[serde(default)]
    manifests: Vec<Descriptor>,
}

pub struct Registry {
    http: reqwest::Client,
    base: String,
    credentials: Option<(String, String)>,
    tokens: Mutex<Vec<(String, String)>>,
}

pub fn digest_of(bytes: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}

impl Registry {
    pub fn new(base: &str, credentials: Option<(String, String)>) -> Result<Self> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30 * 60))
            .build()?;
        Ok(Self { http, base: base.trim_end_matches('/').to_owned(), credentials, tokens: Mutex::new(Vec::new()) })
    }

    pub fn host(&self) -> &str {
        self.base.split_once("://").map(|(_, host)| host).unwrap_or(&self.base)
    }

    async fn send(&self, repository: &str, build: impl Fn() -> RequestBuilder) -> Result<Response> {
        let scope = format!("repository:{repository}:pull,push");
        let cached = self.tokens.lock().await.iter().find(|(s, _)| *s == scope).map(|(_, t)| t.clone());
        let attach = |request: RequestBuilder, token: Option<&str>| match token {
            Some(token) => request.header(AUTHORIZATION, format!("Bearer {token}")),
            None => request,
        };
        let response = attach(build(), cached.as_deref()).send().await?;
        if response.status() != StatusCode::UNAUTHORIZED {
            return Ok(response);
        }
        let token = self.authenticate(response.headers(), &scope).await?;
        let mut tokens = self.tokens.lock().await;
        tokens.retain(|(s, _)| *s != scope);
        tokens.push((scope, token.clone()));
        drop(tokens);
        Ok(attach(build(), Some(&token)).send().await?)
    }

    async fn authenticate(&self, headers: &HeaderMap, scope: &str) -> Result<String> {
        let challenge = headers.get(WWW_AUTHENTICATE).and_then(|v| v.to_str().ok()).unwrap_or_default();
        let params = parse_challenge(challenge);
        let value = |key: &str| params.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());
        let realm = value("realm").ok_or_else(|| Error::Status {
            status: StatusCode::UNAUTHORIZED,
            what: "authentication".into(),
            message: "the registry did not offer a token realm".into(),
        })?;
        let mut query = vec![("scope", scope.to_owned())];
        if let Some(service) = value("service") {
            query.push(("service", service));
        }
        let mut request = self.http.get(&realm).query(&query);
        if let Some((user, password)) = &self.credentials {
            request = request.header(AUTHORIZATION, format!("Basic {}", STANDARD.encode(format!("{user}:{password}"))));
        }
        let response = check(request.send().await?, "authentication").await?;
        let body: serde_json::Value = response.json().await?;
        body.get("token")
            .or_else(|| body.get("access_token"))
            .and_then(|t| t.as_str())
            .map(str::to_owned)
            .ok_or_else(|| Error::Layout("token response without a token".into()))
    }

    pub async fn tags(&self, repository: &str) -> Result<Vec<String>> {
        #[derive(Deserialize)]
        struct Tags {
            tags: Option<Vec<String>>,
        }
        let mut out = Vec::new();
        let mut next = Some(format!("{}/v2/{repository}/tags/list?n=1000", self.base));
        while let Some(url) = next.take() {
            let response = self.send(repository, || self.http.get(&url)).await?;
            if response.status() == StatusCode::NOT_FOUND {
                return Ok(out);
            }
            let response = check(response, &format!("tags of {repository}")).await?;
            next = response
                .headers()
                .get("link")
                .and_then(|v| v.to_str().ok())
                .and_then(|link| link.split_once('<').and_then(|(_, rest)| rest.split_once('>')).map(|(path, _)| path))
                .map(|path| if path.starts_with("http") { path.to_owned() } else { format!("{}{path}", self.base) });
            out.extend(response.json::<Tags>().await?.tags.unwrap_or_default());
        }
        Ok(out)
    }

    pub async fn manifest(&self, repository: &str, reference: &str) -> Result<Option<Manifest>> {
        let url = format!("{}/v2/{repository}/manifests/{reference}", self.base);
        let response = self.send(repository, || self.http.get(&url).header(ACCEPT, MANIFEST_TYPES.join(", "))).await?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let response = check(response, &format!("{repository}:{reference}")).await?;
        let media_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.split(';').next().unwrap_or(v).trim().to_owned())
            .unwrap_or_default();
        let bytes = response.bytes().await?.to_vec();
        let digest = digest_of(&bytes);
        if reference.starts_with("sha256:") && digest != reference {
            return Err(Error::DigestMismatch { what: repository.into(), expected: reference.into(), actual: digest });
        }
        Ok(Some(Manifest { media_type, digest, bytes }))
    }

    pub async fn put_manifest(&self, repository: &str, reference: &str, manifest: &Manifest) -> Result<String> {
        let url = format!("{}/v2/{repository}/manifests/{reference}", self.base);
        let response = self
            .send(repository, || {
                self.http.put(&url).header(CONTENT_TYPE, &manifest.media_type).body(manifest.bytes.clone())
            })
            .await?;
        let response = check(response, &format!("{repository}:{reference}")).await?;
        let returned = response.headers().get("docker-content-digest").and_then(|v| v.to_str().ok()).map(str::to_owned);
        match returned {
            Some(actual) if actual != manifest.digest => Err(Error::DigestMismatch {
                what: format!("{repository}:{reference}"),
                expected: manifest.digest.clone(),
                actual,
            }),
            _ => Ok(manifest.digest.clone()),
        }
    }

    pub async fn promote(&self, repository: &str, digest: &str, tag: &str) -> Result<Promotion> {
        if let Some(existing) = self.manifest(repository, tag).await? {
            if existing.digest == digest {
                return Ok(Promotion::AlreadyThere);
            }
            return Err(Error::TagTaken { repository: repository.into(), tag: tag.into(), existing: existing.digest });
        }
        let manifest = self.manifest(repository, digest).await?.ok_or_else(|| Error::Status {
            status: StatusCode::NOT_FOUND,
            what: format!("{repository}@{digest}"),
            message: "the candidate is no longer in the registry".into(),
        })?;
        self.put_manifest(repository, tag, &manifest).await?;
        Ok(Promotion::Created)
    }

    pub async fn push_layout(&self, repository: &str, layout: &Path, tag: &str) -> Result<String> {
        let index: Links = serde_json::from_slice(&tokio::fs::read(layout.join("index.json")).await?)
            .map_err(|e| Error::Layout(format!("index.json: {e}")))?;
        let [top] = index.manifests.as_slice() else {
            return Err(Error::Layout(format!("expected one image in index.json, found {}", index.manifests.len())));
        };
        self.push_children(repository, layout, top).await?;
        let bytes = read_blob(layout, top).await?;
        let manifest = Manifest { media_type: top.media_type.clone(), digest: top.digest.clone(), bytes };
        self.put_manifest(repository, tag, &manifest).await
    }

    async fn push_children(&self, repository: &str, layout: &Path, descriptor: &Descriptor) -> Result<()> {
        let bytes = read_blob(layout, descriptor).await?;
        let links: Links =
            serde_json::from_slice(&bytes).map_err(|e| Error::Layout(format!("{}: {e}", descriptor.digest)))?;
        for blob in links.config.iter().chain(&links.layers) {
            self.push_blob(repository, layout, blob).await?;
        }
        for child in &links.manifests {
            Box::pin(self.push_children(repository, layout, child)).await?;
            let bytes = read_blob(layout, child).await?;
            let manifest = Manifest { media_type: child.media_type.clone(), digest: child.digest.clone(), bytes };
            self.put_manifest(repository, &child.digest, &manifest).await?;
        }
        Ok(())
    }

    async fn push_blob(&self, repository: &str, layout: &Path, blob: &Descriptor) -> Result<()> {
        let blob_url = format!("{}/v2/{repository}/blobs/{}", self.base, blob.digest);
        if self.send(repository, || self.http.head(&blob_url)).await?.status().is_success() {
            return Ok(());
        }
        let start = format!("{}/v2/{repository}/blobs/uploads/", self.base);
        let response =
            self.send(repository, || self.http.request(Method::POST, &start).header(CONTENT_LENGTH, 0)).await?;
        let response = check(response, &format!("upload to {repository}")).await?;
        let location = response
            .headers()
            .get(LOCATION)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| Error::Layout("upload without a location".into()))?;
        let location =
            if location.starts_with("http") { location.to_owned() } else { format!("{}{location}", self.base) };
        let separator = if location.contains('?') { '&' } else { '?' };
        let target = format!("{location}{separator}digest={}", blob.digest);
        let path = blob_path(layout, &blob.digest)?;
        let response = self
            .send(repository, || {
                let body = match std::fs::File::open(&path) {
                    Ok(file) => {
                        reqwest::Body::wrap_stream(tokio_util::io::ReaderStream::new(tokio::fs::File::from_std(file)))
                    }
                    Err(_) => reqwest::Body::from(Vec::new()),
                };
                self.http
                    .put(&target)
                    .header(CONTENT_TYPE, "application/octet-stream")
                    .header(CONTENT_LENGTH, blob.size)
                    .body(body)
            })
            .await?;
        check(response, &format!("blob {}", blob.digest)).await?;
        Ok(())
    }
}

fn blob_path(layout: &Path, digest: &str) -> Result<PathBuf> {
    let (algorithm, hex) = digest.split_once(':').ok_or_else(|| Error::Layout(format!("bad digest {digest}")))?;
    if algorithm != "sha256" || hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(Error::Layout(format!("bad digest {digest}")));
    }
    Ok(layout.join("blobs").join(algorithm).join(hex))
}

async fn read_blob(layout: &Path, descriptor: &Descriptor) -> Result<Vec<u8>> {
    let bytes = tokio::fs::read(blob_path(layout, &descriptor.digest)?).await?;
    let actual = digest_of(&bytes);
    if actual != descriptor.digest {
        return Err(Error::DigestMismatch { what: "layout blob".into(), expected: descriptor.digest.clone(), actual });
    }
    Ok(bytes)
}

async fn check(response: Response, what: &str) -> Result<Response> {
    if response.status().is_success() {
        return Ok(response);
    }
    let status = response.status();
    let message = response.text().await.unwrap_or_default();
    Err(Error::Status { status, what: what.to_owned(), message: message.chars().take(500).collect() })
}

fn parse_challenge(header: &str) -> Vec<(String, String)> {
    let Some(params) = header.strip_prefix("Bearer ").or_else(|| header.strip_prefix("bearer ")) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut rest = params;
    while let Some((key, after)) = rest.split_once('=') {
        let key = key.trim().trim_start_matches(',').trim().to_owned();
        let after = after.trim_start();
        let (value, remaining) = match after.strip_prefix('"') {
            Some(quoted) => {
                let end = quoted.find('"').unwrap_or(quoted.len());
                (&quoted[..end], quoted.get(end + 1..).unwrap_or_default())
            }
            None => {
                let end = after.find(',').unwrap_or(after.len());
                (&after[..end], &after[end..])
            }
        };
        out.push((key, value.to_owned()));
        rest = remaining;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bearer_challenges_are_parsed() {
        let params = parse_challenge(
            r#"Bearer realm="https://ghcr.io/token",service="ghcr.io",scope="repository:o/app:pull,push""#,
        );
        assert_eq!(
            params,
            vec![
                ("realm".into(), "https://ghcr.io/token".into()),
                ("service".into(), "ghcr.io".into()),
                ("scope".into(), "repository:o/app:pull,push".into()),
            ]
        );
    }

    #[test]
    fn blob_paths_cannot_escape_the_layout() {
        assert!(blob_path(Path::new("/l"), "sha256:../../etc/passwd").is_err());
        assert!(blob_path(Path::new("/l"), &format!("sha512:{}", "a".repeat(64))).is_err());
        assert!(blob_path(Path::new("/l"), &format!("sha256:{}", "a".repeat(64))).is_ok());
    }
}
