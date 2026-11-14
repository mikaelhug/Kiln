use std::future::Future;

use anyhow::Result;
use futures::stream::BoxStream;
use kiln_github::{CheckRun, Conditional, Head, PullRequest, TagRef, Token};
use kiln_registry::Promotion;
use kiln_runner::k8s_openapi::api::core::v1::Pod;
use kiln_store::Repo;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoInfo {
    pub id: i64,
    pub installation_id: i64,
    pub owner: String,
    pub name: String,
    pub default_branch: String,
    pub private: bool,
    pub archived: bool,
}

#[derive(Debug, Clone)]
pub enum PodEvent {
    Applied(Box<Pod>),
    Deleted(String),
    Restarted,
}

pub trait Platform: Send + Sync + 'static {
    fn repositories(&self) -> impl Future<Output = Result<Vec<RepoInfo>>> + Send;
    fn head(&self, repo: &Repo, etag: Option<&str>) -> impl Future<Output = Result<Conditional<Head>>> + Send;
    fn tags(&self, repo: &Repo, etag: Option<&str>) -> impl Future<Output = Result<Conditional<Vec<TagRef>>>> + Send;
    fn tag_commit(&self, repo: &Repo, tag: &TagRef) -> impl Future<Output = Result<String>> + Send;
    fn pulls(
        &self,
        repo: &Repo,
        etag: Option<&str>,
    ) -> impl Future<Output = Result<Conditional<Vec<PullRequest>>>> + Send;
    fn file(&self, repo: &Repo, path: &str, sha: &str) -> impl Future<Output = Result<Option<String>>> + Send;
    fn commit_message(&self, repo: &Repo, sha: &str) -> impl Future<Output = Result<String>> + Send;
    fn on_default_branch(&self, repo: &Repo, sha: &str) -> impl Future<Output = Result<bool>> + Send;
    fn fetch_token(&self, repo: &Repo) -> impl Future<Output = Result<Token>> + Send;
    fn revoke(&self, token: &str) -> impl Future<Output = Result<()>> + Send;
    fn create_check(&self, repo: &Repo, check: &CheckRun) -> impl Future<Output = Result<i64>> + Send;
    fn update_check(&self, repo: &Repo, id: i64, check: &CheckRun) -> impl Future<Output = Result<()>> + Send;

    fn registry_tags(&self, repository: &str) -> impl Future<Output = Result<Vec<String>>> + Send;
    fn promote(&self, repository: &str, digest: &str, tag: &str) -> impl Future<Output = Result<Promotion>> + Send;

    fn ensure_cache(&self, claim: &str) -> impl Future<Output = Result<()>> + Send;
    fn delete_cache(&self, claim: &str) -> impl Future<Output = Result<()>> + Send;
    fn create_secret(&self, name: &str, build: i64, key: &str, value: &str) -> impl Future<Output = Result<()>> + Send;
    fn delete_secret(&self, name: &str) -> impl Future<Output = Result<()>> + Send;
    fn build_secrets(&self) -> impl Future<Output = Result<Vec<(String, i64)>>> + Send;
    fn create_pod(&self, pod: &Pod) -> impl Future<Output = Result<()>> + Send;
    fn delete_pod(&self, name: &str) -> impl Future<Output = Result<()>> + Send;
    fn pods(&self) -> impl Future<Output = Result<Vec<Pod>>> + Send;
    fn watch(&self) -> BoxStream<'static, PodEvent>;
    fn logs(&self, pod: &str, container: &str) -> impl Future<Output = Result<String>> + Send;
    fn follow(
        &self,
        pod: &str,
        container: &str,
    ) -> impl Future<Output = Result<BoxStream<'static, Result<Vec<u8>>>>> + Send;
}
