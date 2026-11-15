use std::collections::BTreeMap;

use anyhow::{Context, Result};
use futures::stream::BoxStream;
use futures::{AsyncReadExt, StreamExt};
use kiln_github::{CheckRun, Client, Compare, Conditional, Head, PullRequest, TagRef, Token};
use kiln_registry::{Promotion, Registry};
use kiln_runner::k8s_openapi::api::core::v1::{PersistentVolumeClaim, Pod, Secret};
use kiln_runner::{BUILD_LABEL, MANAGED_BY};
use kiln_store::Repo;
use kube::api::{DeleteParams, ListParams, LogParams, PostParams};
use kube::runtime::{WatchStreamExt, watcher};
use kube::{Api, ResourceExt};

use super::platform::{ContainerUsage, Platform, PodEvent, PodRejected, RepoInfo};
use super::settings::{PULL_SECRET, PUSH_SECRET, Settings};

const SERVICE_ACCOUNT: &str = "/var/run/secrets/kubernetes.io/serviceaccount";

fn kubelet_client() -> Result<reqwest::Client> {
    let ca = std::fs::read(format!("{SERVICE_ACCOUNT}/ca.crt")).context("reading the cluster CA")?;
    let certificates = reqwest::Certificate::from_pem_bundle(&ca).context("parsing the cluster CA")?;
    Ok(reqwest::Client::builder().timeout(std::time::Duration::from_secs(10)).tls_certs_only(certificates).build()?)
}

pub struct Live {
    github: Client,
    registry: Registry,
    pods: Api<Pod>,
    kubelet: reqwest::Client,
    namespace: String,
    secrets: Api<Secret>,
    claims: Api<PersistentVolumeClaim>,
    cache_class: Option<String>,
    cache_size: String,
    registry_secrets: Option<String>,
}

const SELECTOR: &str = "app.kubernetes.io/managed-by=kiln";

fn is_status(error: &kube::Error, code: u16) -> bool {
    matches!(error, kube::Error::Api(status) if status.code == code)
}

impl Live {
    pub async fn connect(settings: &Settings) -> Result<Self> {
        let key = kiln_github::AppKey::from_pem(settings.app_id, &settings.app_key)
            .context("reading the GitHub App private key")?;
        let github = Client::new(&settings.github_api, key)?;
        let registry = Registry::new(
            &settings.registry_url,
            settings.registry_password.clone().map(|token| (settings.registry_username.clone(), token)),
        )?;
        let kube = kube::Client::try_default().await.context("connecting to the Kubernetes API")?;
        let namespace = &settings.runner.namespace;
        let mut registry_secrets = None;
        if let Some(token) = &settings.registry_password {
            let secrets: Api<Secret> = Api::namespaced(kube.clone(), namespace);
            let host = settings.registry.split('/').next().unwrap_or_default();
            let auth = base64_engine(&format!("{}:{token}", settings.registry_username));
            let docker = serde_json::json!({ "auths": { host: { "username": settings.registry_username, "password": token, "auth": auth } } });
            for (name, kind, data) in [
                (
                    PUSH_SECRET,
                    "Opaque",
                    serde_json::json!({ "username": settings.registry_username, "password": token }),
                ),
                (
                    PULL_SECRET,
                    "kubernetes.io/dockerconfigjson",
                    serde_json::json!({ ".dockerconfigjson": docker.to_string() }),
                ),
            ] {
                if let Err(error) = put_secret(&secrets, name, kind, data).await {
                    registry_secrets = Some(format!("{name}: {error:#}"));
                    tracing::error!(
                        error = format!("{error:#}"),
                        name,
                        "writing a registry secret failed; image builds cannot publish"
                    );
                }
            }
        }
        Ok(Self {
            github,
            registry,
            pods: Api::namespaced(kube.clone(), namespace),
            kubelet: kubelet_client()?,
            namespace: namespace.clone(),
            secrets: Api::namespaced(kube.clone(), namespace),
            claims: Api::namespaced(kube, namespace),
            cache_class: settings.cache_class.clone(),
            cache_size: settings.cache_size.clone(),
            registry_secrets,
        })
    }

    fn installation(repo: &Repo) -> u64 {
        repo.installation_id as u64
    }
}

fn base64_engine(text: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(text)
}

async fn put_secret(secrets: &Api<Secret>, name: &str, kind: &str, data: serde_json::Value) -> Result<()> {
    let secret: Secret = serde_json::from_value(serde_json::json!({
        "metadata": { "name": name, "labels": labels(None) },
        "type": kind,
        "stringData": data,
    }))?;
    match secrets.create(&PostParams::default(), &secret).await {
        Ok(_) => return Ok(()),
        Err(error) if is_status(&error, 409) => {}
        Err(error) => return Err(error).with_context(|| format!("creating the {name} secret")),
    }
    secrets.delete(name, &DeleteParams::default()).await.with_context(|| format!("replacing the {name} secret"))?;
    secrets.create(&PostParams::default(), &secret).await.with_context(|| format!("creating the {name} secret"))?;
    Ok(())
}

fn labels(build: Option<i64>) -> BTreeMap<String, String> {
    let mut labels = BTreeMap::from([(MANAGED_BY.to_owned(), "kiln".to_owned())]);
    if let Some(build) = build {
        labels.insert(BUILD_LABEL.to_owned(), build.to_string());
    }
    labels
}

impl Platform for Live {
    fn status(&self) -> Vec<(&'static str, String)> {
        let rate = self.github.rate_limit();
        let remaining = match (rate.remaining, rate.reset) {
            (Some(remaining), Some(reset)) => {
                format!(
                    "{remaining} requests left, resets in {} min",
                    ((reset as i64 - kiln_github::unix_now()) / 60).max(0)
                )
            }
            _ => "not measured yet".into(),
        };
        let secrets = self.registry_secrets.clone().map_or_else(|| "ok".into(), |error| format!("failed, {error}"));
        vec![("GitHub API", remaining), ("Registry secrets", secrets)]
    }

    fn registry_secrets_error(&self) -> Option<String> {
        self.registry_secrets.clone()
    }

    async fn repositories(&self) -> Result<Vec<RepoInfo>> {
        let mut out = Vec::new();
        for installation in self.github.installations().await? {
            for repo in self.github.repositories(installation.id).await? {
                out.push(RepoInfo {
                    id: repo.id as i64,
                    installation_id: installation.id as i64,
                    owner: repo.owner.login,
                    name: repo.name,
                    default_branch: repo.default_branch,
                    private: repo.private,
                    archived: repo.archived,
                });
            }
        }
        Ok(out)
    }

    async fn head(&self, repo: &Repo, etag: Option<&str>) -> Result<Conditional<Head>> {
        Ok(self.github.head(Self::installation(repo), &repo.full_name(), &repo.default_branch, etag).await?)
    }

    async fn tags(&self, repo: &Repo, etag: Option<&str>) -> Result<Conditional<Vec<TagRef>>> {
        Ok(self.github.version_tags(Self::installation(repo), &repo.full_name(), etag).await?)
    }

    async fn tag_commit(&self, repo: &Repo, tag: &TagRef) -> Result<String> {
        Ok(self.github.tag_commit(Self::installation(repo), &repo.full_name(), tag).await?)
    }

    async fn pulls(&self, repo: &Repo, etag: Option<&str>) -> Result<Conditional<Vec<PullRequest>>> {
        Ok(self.github.open_pulls(Self::installation(repo), &repo.full_name(), etag).await?)
    }

    async fn file(&self, repo: &Repo, path: &str, sha: &str) -> Result<Option<String>> {
        let limit = kiln_core::config::MAX_FILE_BYTES;
        Ok(self.github.file(Self::installation(repo), &repo.full_name(), path, sha, limit).await?)
    }

    async fn commit_message(&self, repo: &Repo, sha: &str) -> Result<String> {
        Ok(self.github.commit_message(Self::installation(repo), &repo.full_name(), sha).await?)
    }

    async fn on_default_branch(&self, repo: &Repo, sha: &str) -> Result<bool> {
        let status =
            self.github.compare(Self::installation(repo), &repo.full_name(), &repo.default_branch, sha).await?;
        Ok(matches!(status, Compare::Identical | Compare::Behind))
    }

    async fn fetch_token(&self, repo: &Repo) -> Result<Token> {
        Ok(self.github.scoped_token(Self::installation(repo), repo.id as u64, &[("contents", "read")]).await?)
    }

    async fn revoke(&self, token: &str) -> Result<()> {
        Ok(self.github.revoke(token).await?)
    }

    async fn create_check(&self, repo: &Repo, check: &CheckRun) -> Result<i64> {
        Ok(self.github.create_check(Self::installation(repo), &repo.full_name(), check).await? as i64)
    }

    async fn update_check(&self, repo: &Repo, id: i64, check: &CheckRun) -> Result<()> {
        Ok(self.github.update_check(Self::installation(repo), &repo.full_name(), id as u64, check).await?)
    }

    async fn registry_tags(&self, repository: &str) -> Result<Vec<String>> {
        Ok(self.registry.tags(repository).await?)
    }

    async fn promote(&self, repository: &str, digest: &str, tag: &str) -> Result<Promotion> {
        Ok(self.registry.promote(repository, digest, tag).await?)
    }

    async fn ensure_cache(&self, claim: &str) -> Result<()> {
        if self.claims.get_opt(claim).await?.is_some() {
            return Ok(());
        }
        let mut spec = serde_json::json!({
            "accessModes": ["ReadWriteOnce"],
            "resources": { "requests": { "storage": self.cache_size } },
        });
        if let Some(class) = &self.cache_class {
            spec["storageClassName"] = serde_json::json!(class);
        }
        let claim_object: PersistentVolumeClaim = serde_json::from_value(serde_json::json!({
            "metadata": { "name": claim, "labels": labels(None) },
            "spec": spec,
        }))?;
        match self.claims.create(&PostParams::default(), &claim_object).await {
            Ok(_) => Ok(()),
            Err(error) if is_status(&error, 409) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    async fn delete_cache(&self, claim: &str) -> Result<()> {
        match self.claims.delete(claim, &DeleteParams::default()).await {
            Ok(_) => Ok(()),
            Err(error) if is_status(&error, 404) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    async fn create_secret(&self, name: &str, build: i64, key: &str, value: &str) -> Result<()> {
        let secret: Secret = serde_json::from_value(serde_json::json!({
            "metadata": { "name": name, "labels": labels(Some(build)) },
            "type": "Opaque",
            "stringData": { key: value },
        }))?;
        match self.secrets.create(&PostParams::default(), &secret).await {
            Ok(_) => Ok(()),
            Err(error) if is_status(&error, 409) => {
                self.delete_secret(name).await?;
                self.secrets.create(&PostParams::default(), &secret).await?;
                Ok(())
            }
            Err(error) => Err(error.into()),
        }
    }

    async fn delete_secret(&self, name: &str) -> Result<()> {
        match self.secrets.delete(name, &DeleteParams::default()).await {
            Ok(_) => Ok(()),
            Err(error) if is_status(&error, 404) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    async fn pod_usage(&self) -> Result<Vec<ContainerUsage>> {
        let pods = self.pods.list(&ListParams::default().labels(&format!("{MANAGED_BY}=kiln"))).await?;
        let nodes: std::collections::BTreeSet<String> =
            pods.items.iter().filter_map(|p| p.status.as_ref()?.host_ip.clone()).collect();
        let token =
            std::fs::read_to_string(format!("{SERVICE_ACCOUNT}/token")).context("reading the service account token")?;
        let mut out = Vec::new();
        for node in nodes {
            let summary: serde_json::Value = self
                .kubelet
                .get(format!("https://{node}:10250/stats/summary"))
                .bearer_auth(token.trim())
                .send()
                .await
                .and_then(reqwest::Response::error_for_status)
                .with_context(|| format!("reading usage from the kubelet on {node}"))?
                .json()
                .await?;
            for pod in summary["pods"].as_array().into_iter().flatten() {
                if pod["podRef"]["namespace"].as_str() != Some(self.namespace.as_str()) {
                    continue;
                }
                let name = pod["podRef"]["name"].as_str().unwrap_or_default();
                for container in pod["containers"].as_array().into_iter().flatten() {
                    let at = container["cpu"]["time"]
                        .as_str()
                        .and_then(|t| t.parse::<kiln_runner::k8s_openapi::jiff::Timestamp>().ok())
                        .map_or_else(|| kiln_github::unix_now() as i64, |t| t.as_second());
                    out.push(ContainerUsage {
                        at,
                        pod: name.to_owned(),
                        container: container["name"].as_str().unwrap_or_default().to_owned(),
                        cpu: container["cpu"]["usageNanoCores"].as_f64().unwrap_or_default() / 1e9,
                        memory: container["memory"]["workingSetBytes"].as_f64().unwrap_or_default(),
                    });
                }
            }
        }
        Ok(out)
    }

    async fn create_pod(&self, pod: &Pod) -> Result<()> {
        match self.pods.create(&PostParams::default(), pod).await {
            Ok(_) => Ok(()),
            Err(error) if is_status(&error, 409) => Ok(()),
            Err(kube::Error::Api(status)) if status.code == 422 => Err(PodRejected(status.message).into()),
            Err(error) => Err(error.into()),
        }
    }

    async fn delete_pod(&self, name: &str) -> Result<()> {
        let params = DeleteParams { grace_period_seconds: Some(10), ..DeleteParams::default() };
        match self.pods.delete(name, &params).await {
            Ok(_) => Ok(()),
            Err(error) if is_status(&error, 404) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    async fn pods(&self) -> Result<Vec<Pod>> {
        Ok(self.pods.list(&ListParams::default().labels(SELECTOR)).await?.items)
    }

    fn watch(&self) -> BoxStream<'static, PodEvent> {
        watcher(self.pods.clone(), watcher::Config::default().labels(SELECTOR))
            .default_backoff()
            .filter_map(|event| async move {
                match event {
                    Ok(watcher::Event::Apply(pod) | watcher::Event::InitApply(pod)) => {
                        Some(PodEvent::Applied(Box::new(pod)))
                    }
                    Ok(watcher::Event::Delete(pod)) => Some(PodEvent::Deleted(pod.name_any())),
                    Ok(watcher::Event::InitDone) => Some(PodEvent::Restarted),
                    Ok(watcher::Event::Init) => None,
                    Err(error) => {
                        tracing::warn!(%error, "watching build pods failed, retrying");
                        None
                    }
                }
            })
            .boxed()
    }

    async fn logs(&self, pod: &str, container: &str) -> Result<String> {
        let params = LogParams { container: Some(container.to_owned()), ..LogParams::default() };
        let mut reader = match self.pods.log_stream(pod, &params).await {
            Ok(reader) => Box::pin(reader),
            Err(error) if is_status(&error, 404) || is_status(&error, 400) => return Ok(String::new()),
            Err(error) => return Err(error.into()),
        };
        let half = (super::logs::MAX_LOG_BYTES / 2) as usize - 1024;
        let (mut head, mut tail, mut skipped) = (Vec::new(), Vec::new(), 0usize);
        let mut buffer = vec![0u8; 64 * 1024];
        loop {
            let read = reader.read(&mut buffer).await?;
            if read == 0 {
                break;
            }
            let mut chunk = &buffer[..read];
            if head.len() < half {
                let take = chunk.len().min(half - head.len());
                head.extend_from_slice(&chunk[..take]);
                chunk = &chunk[take..];
            }
            tail.extend_from_slice(chunk);
            if tail.len() > 2 * half {
                let drop = tail.len() - half;
                tail.drain(..drop);
                skipped += drop;
            }
        }
        if tail.len() > half {
            let drop = tail.len() - half;
            tail.drain(..drop);
            skipped += drop;
        }
        if skipped > 0 {
            let drop = tail.iter().position(|b| *b == b'\n').map_or(tail.len(), |i| i + 1);
            tail.drain(..drop);
            skipped += drop;
        }
        let mut text = String::from_utf8_lossy(&head).into_owned();
        if skipped > 0 {
            text.push_str(&format!("\n… {skipped} bytes of log omitted …\n"));
        }
        text.push_str(&String::from_utf8_lossy(&tail));
        Ok(text)
    }

    async fn follow(&self, pod: &str, container: &str) -> Result<BoxStream<'static, Result<Vec<u8>>>> {
        let params = LogParams { container: Some(container.to_owned()), follow: true, ..LogParams::default() };
        let reader = self.pods.log_stream(pod, &params).await?;
        let stream = futures::stream::unfold(Box::pin(reader), |mut reader| async move {
            let mut buffer = vec![0u8; 16 * 1024];
            match reader.read(&mut buffer).await {
                Ok(0) => None,
                Ok(read) => {
                    buffer.truncate(read);
                    Some((Ok(buffer), reader))
                }
                Err(error) => Some((Err(anyhow::Error::from(error)), reader)),
            }
        });
        Ok(stream.boxed())
    }
}
