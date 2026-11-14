use std::collections::BTreeMap;

use anyhow::{Context, Result};
use futures::stream::BoxStream;
use futures::{AsyncBufReadExt, StreamExt, TryStreamExt};
use kiln_github::{CheckRun, Client, Compare, Conditional, Head, PullRequest, TagRef, Token};
use kiln_registry::{Promotion, Registry};
use kiln_runner::k8s_openapi::api::core::v1::{PersistentVolumeClaim, Pod, Secret};
use kiln_runner::{BUILD_LABEL, MANAGED_BY};
use kiln_store::Repo;
use kube::api::{DeleteParams, ListParams, LogParams, PostParams};
use kube::runtime::{WatchStreamExt, watcher};
use kube::{Api, ResourceExt};

use super::platform::{Platform, PodEvent, RepoInfo};
use super::settings::Settings;

pub struct Live {
    github: Client,
    registry: Registry,
    pods: Api<Pod>,
    secrets: Api<Secret>,
    claims: Api<PersistentVolumeClaim>,
    cache_class: Option<String>,
    cache_size: String,
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
            Some((settings.registry_username.clone(), settings.registry_password.clone())),
        )?;
        let kube = kube::Client::try_default().await.context("connecting to the Kubernetes API")?;
        let namespace = &settings.runner.namespace;
        Ok(Self {
            github,
            registry,
            pods: Api::namespaced(kube.clone(), namespace),
            secrets: Api::namespaced(kube.clone(), namespace),
            claims: Api::namespaced(kube, namespace),
            cache_class: settings.cache_class.clone(),
            cache_size: settings.cache_size.clone(),
        })
    }

    fn installation(repo: &Repo) -> u64 {
        repo.installation_id as u64
    }
}

fn labels(build: Option<i64>) -> BTreeMap<String, String> {
    let mut labels = BTreeMap::from([(MANAGED_BY.to_owned(), "kiln".to_owned())]);
    if let Some(build) = build {
        labels.insert(BUILD_LABEL.to_owned(), build.to_string());
    }
    labels
}

impl Platform for Live {
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
        Ok(self.github.file(Self::installation(repo), &repo.full_name(), path, sha).await?)
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
                self.secrets.replace(name, &PostParams::default(), &secret).await?;
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

    async fn build_secrets(&self) -> Result<Vec<(String, i64)>> {
        let list = self.secrets.list_metadata(&ListParams::default().labels(SELECTOR)).await?;
        Ok(list
            .items
            .into_iter()
            .filter_map(|secret| {
                let build = secret.labels().get(BUILD_LABEL)?.parse().ok()?;
                Some((secret.name_any(), build))
            })
            .collect())
    }

    async fn create_pod(&self, pod: &Pod) -> Result<()> {
        match self.pods.create(&PostParams::default(), pod).await {
            Ok(_) => Ok(()),
            Err(error) if is_status(&error, 409) => Ok(()),
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
        match self.pods.logs(pod, &params).await {
            Ok(text) => Ok(text),
            Err(error) if is_status(&error, 404) || is_status(&error, 400) => Ok(String::new()),
            Err(error) => Err(error.into()),
        }
    }

    async fn follow(&self, pod: &str, container: &str) -> Result<BoxStream<'static, Result<Vec<u8>>>> {
        let params = LogParams { container: Some(container.to_owned()), follow: true, ..LogParams::default() };
        let reader = self.pods.log_stream(pod, &params).await?;
        Ok(reader.lines().map_ok(|line| format!("{line}\n").into_bytes()).map_err(anyhow::Error::from).boxed())
    }
}
