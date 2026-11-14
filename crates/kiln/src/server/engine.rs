use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use futures::StreamExt;
use kiln_core::config::{self, Config, FILE_NAME, ReleaseMode};
use kiln_core::schedule::{self, BuildId, RepoId};
use kiln_core::trust::{Origin, grant_secrets};
use kiln_core::version::{self, Publish};
use kiln_github::{Conditional, unix_now};
use kiln_registry::Promotion;
use kiln_runner::k8s_openapi::api::core::v1::Pod;
use kiln_runner::{ATTEMPT_LABEL, BUILD_LABEL, Marker, Outcome, Plan, Scope, cache_claim_name, parse_marker, pod_name};
use kiln_store::{
    AuditEntry, Build, BuildFilter, BuildState, BuildUpdate, EtagKind, Failure, NewBuild, Phase, Repo, RepoSync,
    Retention, StepUpdate, Store, Task, TaskKind, TaskState, TaskUpdate, Trigger,
};
use tokio::sync::{Notify, mpsc};

use super::logs::{Event, Logs};
use super::platform::{Platform, PodEvent, PodRejected};
use super::settings::Settings;

const PULL_GRACE_SECS: i64 = 180;
const PENDING_LIMIT_SECS: i64 = 15 * 60;

pub struct Engine<P: Platform> {
    pub store: Arc<Store>,
    pub platform: Arc<P>,
    pub settings: Arc<Settings>,
    pub logs: Arc<Logs>,
    scheduler: Notify,
    outbox: Notify,
    reconcile_tx: mpsc::UnboundedSender<Option<i64>>,
    reconcile_rx: Mutex<Option<mpsc::UnboundedReceiver<Option<i64>>>>,
    followed: Mutex<HashSet<(String, String)>>,
    finalized: Mutex<HashSet<(i64, i64, i64)>>,
    fetch_tokens: Mutex<HashMap<(i64, i64), String>>,
    releasing: Mutex<HashSet<i64>>,
    dispatching: Mutex<HashSet<i64>>,
}

fn now() -> i64 {
    unix_now()
}

fn backoff(base: i64, attempt: i64) -> i64 {
    base * 2i64.pow((attempt - 1).clamp(0, 6) as u32)
}

impl<P: Platform> Engine<P> {
    pub fn new(store: Arc<Store>, platform: Arc<P>, settings: Arc<Settings>, logs: Arc<Logs>) -> Arc<Self> {
        let (reconcile_tx, reconcile_rx) = mpsc::unbounded_channel();
        Arc::new(Self {
            store,
            platform,
            settings,
            logs,
            scheduler: Notify::new(),
            outbox: Notify::new(),
            reconcile_tx,
            reconcile_rx: Mutex::new(Some(reconcile_rx)),
            followed: Mutex::new(HashSet::new()),
            finalized: Mutex::new(HashSet::new()),
            fetch_tokens: Mutex::new(HashMap::new()),
            releasing: Mutex::new(HashSet::new()),
            dispatching: Mutex::new(HashSet::new()),
        })
    }

    pub fn start(self: &Arc<Self>) {
        let engine = self.clone();
        tokio::spawn(async move { engine.reconcile_loop().await });
        let engine = self.clone();
        tokio::spawn(async move { engine.watch_loop().await });
        let engine = self.clone();
        tokio::spawn(async move { engine.scheduler_loop().await });
        let engine = self.clone();
        tokio::spawn(async move { engine.outbox_loop().await });
        let engine = self.clone();
        tokio::spawn(async move { engine.housekeeping_loop().await });
    }

    pub fn poke(&self, repo: Option<i64>) {
        let _ = self.reconcile_tx.send(repo);
    }

    fn changed(&self, build: &Build) {
        let _ = self.logs.events().send(Event::Build { build: build.id, repo: build.repo_id });
        if let Err(error) = self.store.enqueue(build.id, "check", &serde_json::json!({}), now()) {
            tracing::warn!(%error, build = build.id, "queueing the GitHub check update failed");
        }
        self.outbox.notify_one();
    }

    fn update(&self, id: i64, update: BuildUpdate) -> Result<Build> {
        self.store.update_build(id, &update)?;
        let build = self.store.build(id)?.ok_or_else(|| anyhow!("build {id} disappeared"))?;
        self.changed(&build);
        Ok(build)
    }

    fn finish(&self, id: i64, state: BuildState, failure: Option<Failure>, detail: Option<String>) -> Result<Build> {
        self.update(
            id,
            BuildUpdate {
                state: Some(state),
                failure: Some(failure),
                detail: Some(detail),
                finished_at: Some(now()),
                ..BuildUpdate::default()
            },
        )
    }

    fn audit(&self, actor: &str, action: &str, repo: Option<i64>, build: Option<i64>, detail: Option<String>) {
        let entry = AuditEntry {
            at: now(),
            actor: actor.into(),
            action: action.into(),
            repo_id: repo,
            build_id: build,
            detail,
        };
        tracing::info!(actor, action, repo, build, "audit");
        if let Err(error) = self.store.audit(&entry) {
            tracing::warn!(%error, "writing the audit log failed");
        }
    }

    async fn reconcile_loop(self: Arc<Self>) {
        let mut rx = self.reconcile_rx.lock().expect("reconcile receiver").take().expect("reconcile loop runs once");
        let mut tick = tokio::time::interval(Duration::from_secs(self.settings.poll_secs));
        let mut last_full = 0;
        loop {
            let target = tokio::select! {
                _ = tick.tick() => None,
                Some(target) = rx.recv() => target,
            };
            let result = match target {
                Some(repo) => self.reconcile_repo_id(repo).await,
                None => {
                    let full = now() - last_full > 600;
                    if full {
                        last_full = now();
                    }
                    self.reconcile_all(full).await
                }
            };
            if let Err(error) = result {
                tracing::warn!(error = format!("{error:#}"), "reconciling with GitHub failed");
            }
            self.scheduler.notify_one();
        }
    }

    pub async fn reconcile_all(&self, discover: bool) -> Result<()> {
        if discover || self.store.repos()?.is_empty() {
            let repos = self.platform.repositories().await?;
            let mut keep = Vec::new();
            for repo in repos {
                let owner_allowed = self.settings.owners.iter().any(|o| o.eq_ignore_ascii_case(&repo.owner));
                if !owner_allowed || repo.archived || (!repo.private && !self.settings.include_public) {
                    continue;
                }
                keep.push(repo.id);
                if let Err(error) = self.store.upsert_repo(&RepoSync {
                    id: repo.id,
                    installation_id: repo.installation_id,
                    owner: repo.owner,
                    name: repo.name,
                    default_branch: repo.default_branch,
                    private: repo.private,
                    archived: repo.archived,
                    seen_at: now(),
                }) {
                    tracing::warn!(%error, "recording a repository failed");
                }
            }
            self.store.mark_removed(&keep, now())?;
        }
        for repo in self.store.repos()?.into_iter().filter(|r| r.removed_at.is_none()) {
            if let Err(error) = self.reconcile_repo(&repo).await {
                tracing::warn!(
                    repo = repo.full_name(),
                    error = format!("{error:#}"),
                    "reconciling the repository failed"
                );
            }
        }
        Ok(())
    }

    async fn reconcile_repo_id(&self, id: i64) -> Result<()> {
        match self.store.repo(id)? {
            Some(repo) => self.reconcile_repo(&repo).await,
            None => self.reconcile_all(true).await,
        }
    }

    async fn reconcile_repo(&self, repo: &Repo) -> Result<()> {
        if repo.paused {
            return Ok(());
        }
        if let Conditional::Fresh { value: head, etag } = self.platform.head(repo, repo.etag_head.as_deref()).await? {
            let config = self.platform.file(repo, FILE_NAME, &head.sha).await?;
            let error = config
                .as_deref()
                .and_then(|text| config::parse(text, &repo.name).err())
                .map(|d| render_diagnostics(&d));
            let newly_enrolled = config.is_some() && !repo.enrolled;
            self.store.set_head(repo.id, &head.sha, config.is_some(), error.as_deref())?;
            if newly_enrolled {
                self.adopt_existing_tags(repo).await?;
                self.audit("kiln", "repository_enrolled", Some(repo.id), None, Some(repo.full_name()));
            }
            if let Some(text) = config {
                let wants = config::parse(&text, &repo.name).map(|c| c.on.push).unwrap_or(true);
                if wants {
                    self.enqueue(
                        repo,
                        Trigger::Push,
                        &repo.default_branch,
                        None,
                        &head.sha,
                        Some(head.message),
                        None,
                        Some(text),
                        None,
                    )?;
                }
            }
            self.store.set_etag(repo.id, EtagKind::Head, etag.as_deref())?;
            let _ = self.logs.events().send(Event::Repo { repo: repo.id });
        }
        let Some(repo) = self.store.repo(repo.id)? else { return Ok(()) };
        if !repo.enrolled {
            return Ok(());
        }
        if let Conditional::Fresh { value: tags, etag } = self.platform.tags(&repo, repo.etag_tags.as_deref()).await? {
            let seen: HashSet<String> = self.store.tags_seen(repo.id)?.into_iter().collect();
            for tag in tags.iter().filter(|t| !seen.contains(&t.name)) {
                let sha = self.platform.tag_commit(&repo, tag).await?;
                if let Some(text) = self.platform.file(&repo, FILE_NAME, &sha).await?
                    && config::parse(&text, &repo.name).map(|c| c.on.tags).unwrap_or(true)
                {
                    let message = self.platform.commit_message(&repo, &sha).await.ok();
                    let on_default_branch = self.platform.on_default_branch(&repo, &sha).await?;
                    self.enqueue_tag(&repo, &tag.name, &sha, message, on_default_branch, text)?;
                }
                self.store.mark_tags_seen(repo.id, std::slice::from_ref(&tag.name))?;
            }
            self.store.set_etag(repo.id, EtagKind::Tags, etag.as_deref())?;
        }
        if let Conditional::Fresh { value: pulls, etag } =
            self.platform.pulls(&repo, repo.etag_pulls.as_deref()).await?
        {
            let open: HashSet<i64> = pulls.iter().map(|p| p.number as i64).collect();
            for pull in pulls.iter().filter(|p| !p.from_fork) {
                if self
                    .store
                    .builds(&BuildFilter {
                        repo: Some(repo.id),
                        sha: Some(pull.head_sha.clone()),
                        limit: 50,
                        ..Default::default()
                    })?
                    .iter()
                    .any(|b| b.pr_number == Some(pull.number as i64))
                {
                    continue;
                }
                let Some(text) = self.platform.file(&repo, FILE_NAME, &pull.head_sha).await? else { continue };
                if !config::parse(&text, &repo.name).map(|c| c.on.pull_request).unwrap_or(true) {
                    continue;
                }
                let message = Some(pull.title.clone());
                self.enqueue(
                    &repo,
                    Trigger::PullRequest,
                    &pull.head_ref,
                    Some(pull.number as i64),
                    &pull.head_sha,
                    message,
                    None,
                    Some(text),
                    None,
                )?;
            }
            let pending = self.store.queued()?.into_iter().chain(self.store.active()?);
            for build in pending.filter(|b| b.repo_id == repo.id && b.trigger == Trigger::PullRequest) {
                if let Some(number) = build.pr_number
                    && !open.contains(&number)
                {
                    self.cancel_build(&build, "Superseded because the pull request was closed")?;
                }
            }
            self.store.set_etag(repo.id, EtagKind::Pulls, etag.as_deref())?;
        }
        Ok(())
    }

    async fn adopt_existing_tags(&self, repo: &Repo) -> Result<()> {
        if let Conditional::Fresh { value, .. } = self.platform.tags(repo, None).await? {
            let names: Vec<String> = value.into_iter().map(|t| t.name).collect();
            self.store.mark_tags_seen(repo.id, &names)?;
        }
        self.store.set_etag(repo.id, EtagKind::Tags, None)?;
        Ok(())
    }

    fn enqueue_tag(
        &self,
        repo: &Repo,
        tag: &str,
        sha: &str,
        message: Option<String>,
        on_default_branch: bool,
        text: String,
    ) -> Result<Option<i64>> {
        let build = NewBuild {
            repo_id: repo.id,
            trigger: Trigger::Tag,
            ref_name: tag.to_owned(),
            pr_number: None,
            sha: sha.to_owned(),
            message,
            dedupe_key: format!("tag:{tag}:{sha}"),
            actor: None,
            trusted: grant_secrets(&Origin::Tag { on_default_branch }),
            config: Some(text),
            state: BuildState::Queued,
            failure: None,
            detail: None,
            rerun_of: None,
            created_at: now(),
        };
        self.insert(repo, build)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn enqueue(
        &self,
        repo: &Repo,
        trigger: Trigger,
        reference: &str,
        pr: Option<i64>,
        sha: &str,
        message: Option<String>,
        actor: Option<String>,
        config_text: Option<String>,
        rerun_of: Option<i64>,
    ) -> Result<Option<i64>> {
        let dedupe_key = match (trigger, rerun_of) {
            (_, Some(original)) => format!("rerun:{original}:{}:{}", now(), kiln_github::random_token()),
            (Trigger::Push, None) => format!("push:{sha}"),
            (Trigger::PullRequest, None) => format!("pr:{}:{sha}", pr.unwrap_or_default()),
            (Trigger::Manual, None) => format!("manual:{sha}:{}", kiln_github::random_token()),
            (Trigger::Tag, None) => format!("tag:{reference}:{sha}"),
        };
        let skip = trigger == Trigger::Push
            && actor.is_none()
            && message.as_deref().is_some_and(|m| {
                let lower = m.to_ascii_lowercase();
                lower.contains("[skip ci]") || lower.contains("[skip kiln]") || lower.contains("[ci skip]")
            });
        let origin = match trigger {
            Trigger::Push => Origin::DefaultBranchPush,
            Trigger::PullRequest => Origin::PullRequest,
            Trigger::Tag => Origin::Tag { on_default_branch: false },
            Trigger::Manual => Origin::Ui {
                default_branch_head: repo.head_sha.as_deref() == Some(sha) || reference == repo.default_branch,
            },
        };
        let build = NewBuild {
            repo_id: repo.id,
            trigger,
            ref_name: reference.to_owned(),
            pr_number: pr,
            sha: sha.to_owned(),
            message,
            dedupe_key,
            trusted: grant_secrets(&origin),
            actor,
            config: config_text,
            state: if skip { BuildState::Skipped } else { BuildState::Queued },
            failure: None,
            detail: skip.then(|| "Skipped because the commit message asks to skip CI".to_owned()),
            rerun_of,
            created_at: now(),
        };
        self.insert(repo, build)
    }

    fn insert(&self, repo: &Repo, mut build: NewBuild) -> Result<Option<i64>> {
        if build.state == BuildState::Queued
            && let Some(text) = &build.config
            && let Err(diagnostics) = config::parse(text, &repo.name)
        {
            build.state = BuildState::Failed;
            build.failure = Some(Failure::Config);
            build.detail = Some(render_diagnostics(&diagnostics));
        }
        if build.config.as_ref().is_some_and(|text| text.len() > config::MAX_FILE_BYTES) {
            build.config = None;
        }
        let Some(id) = self.store.insert_build(&build)? else { return Ok(None) };
        if build.state.is_finished() {
            self.store.update_build(id, &BuildUpdate { finished_at: Some(now()), ..BuildUpdate::default() })?;
        }
        let created = self.store.build(id)?.ok_or_else(|| anyhow!("build {id} disappeared"))?;
        self.changed(&created);
        if created.state == BuildState::Queued && build.rerun_of.is_none() {
            let newer = core_trigger(&created);
            for older in self.store.same_line(&created)?.into_iter().filter(|o| newer.supersedes(&core_trigger(o))) {
                self.cancel_build(&older, &format!("Superseded by #{}", created.number))?;
            }
        }
        self.scheduler.notify_one();
        Ok(Some(id))
    }

    fn cancel_build(&self, build: &Build, reason: &str) -> Result<()> {
        let superseded = reason.starts_with("Superseded");
        match build.state {
            BuildState::Queued => {
                let state = if superseded { BuildState::Superseded } else { BuildState::Cancelled };
                self.finish(build.id, state, None, Some(reason.to_owned()))?;
            }
            BuildState::Starting | BuildState::Running => {
                self.update(
                    build.id,
                    BuildUpdate {
                        cancel_requested: Some(true),
                        detail: Some(Some(reason.to_owned())),
                        ..Default::default()
                    },
                )?;
                let platform = self.platform.clone();
                let pod = build.pod.clone();
                tokio::spawn(async move {
                    if let Some(pod) = pod
                        && let Err(error) = platform.delete_pod(&pod).await
                    {
                        tracing::warn!(%error, pod, "deleting the build pod failed");
                    }
                });
            }
            _ => {}
        }
        Ok(())
    }

    pub fn cancel(&self, id: i64, actor: &str) -> Result<()> {
        let build = self.store.build(id)?.ok_or_else(|| anyhow!("build {id} does not exist"))?;
        if build.state == BuildState::Releasing {
            if !self.releasing.lock().expect("releasing set").insert(id) {
                anyhow::bail!("Kiln is copying this build's images right now; try again in a moment");
            }
            let outcome = (|| {
                if self.store.images(id)?.iter().any(|i| i.released.is_some()) {
                    anyhow::bail!("this build has already released some of its images and cannot be cancelled");
                }
                self.finish(
                    id,
                    BuildState::Cancelled,
                    None,
                    Some(format!("Cancelled by {actor} before anything was released")),
                )
            })();
            self.releasing.lock().expect("releasing set").remove(&id);
            outcome?;
            self.audit(actor, "build_cancelled", Some(build.repo_id), Some(id), None);
            self.scheduler.notify_one();
            return Ok(());
        }
        self.cancel_build(&build, &format!("Cancelled by {actor}"))?;
        self.audit(actor, "build_cancelled", Some(build.repo_id), Some(id), None);
        Ok(())
    }

    pub async fn rerun(&self, id: i64, actor: &str) -> Result<i64> {
        let original = self.store.build(id)?.ok_or_else(|| anyhow!("build {id} does not exist"))?;
        let repo = self.store.repo(original.repo_id)?.ok_or_else(|| anyhow!("the repository is gone"))?;
        if !original.state.is_finished() {
            anyhow::bail!("build #{} is still running", original.number);
        }
        if original.trigger == Trigger::Tag && original.state == BuildState::Succeeded && original.version.is_some() {
            anyhow::bail!("{} is already released; semver tags are immutable", original.version.unwrap_or_default());
        }
        let mut build = NewBuild {
            repo_id: repo.id,
            trigger: original.trigger,
            ref_name: original.ref_name.clone(),
            pr_number: original.pr_number,
            sha: original.sha.clone(),
            message: original.message.clone(),
            dedupe_key: format!("rerun:{}:{}", original.id, kiln_github::random_token()),
            actor: Some(actor.to_owned()),
            trusted: match original.trigger {
                Trigger::Tag => grant_secrets(&Origin::Tag {
                    on_default_branch: self.platform.on_default_branch(&repo, &original.sha).await?,
                }),
                _ => original.trusted,
            },
            config: original.config.clone(),
            state: BuildState::Queued,
            failure: None,
            detail: None,
            rerun_of: Some(original.id),
            created_at: now(),
        };
        if build.trigger == Trigger::Push && repo.head_sha.as_deref() != Some(&original.sha) {
            build.trigger = Trigger::Manual;
        }
        let id = self.insert(&repo, build)?.ok_or_else(|| anyhow!("the rerun could not be created"))?;
        self.audit(actor, "build_rerun", Some(repo.id), Some(id), Some(format!("rerun of #{}", original.number)));
        Ok(id)
    }

    pub async fn trigger(&self, repo_id: i64, actor: &str) -> Result<i64> {
        let repo = self.store.repo(repo_id)?.ok_or_else(|| anyhow!("repository {repo_id} does not exist"))?;
        let head = match self.platform.head(&repo, None).await? {
            Conditional::Fresh { value, .. } => value,
            Conditional::NotModified => anyhow::bail!("GitHub did not return the default branch"),
        };
        let text = self
            .platform
            .file(&repo, FILE_NAME, &head.sha)
            .await?
            .ok_or_else(|| anyhow!("{} has no {FILE_NAME} on {}", repo.full_name(), repo.default_branch))?;
        let id = self
            .enqueue(
                &repo,
                Trigger::Manual,
                &repo.default_branch,
                None,
                &head.sha,
                Some(head.message),
                Some(actor.to_owned()),
                Some(text),
                None,
            )?
            .ok_or_else(|| anyhow!("the build could not be created"))?;
        self.audit(actor, "build_triggered", Some(repo.id), Some(id), None);
        Ok(id)
    }

    pub fn set_paused(&self, repo: i64, paused: bool, actor: &str) -> Result<()> {
        self.store.set_paused(repo, paused)?;
        if !paused {
            self.store.clear_etags(repo)?;
            self.poke(Some(repo));
        }
        self.audit(actor, if paused { "repository_paused" } else { "repository_resumed" }, Some(repo), None, None);
        let _ = self.logs.events().send(Event::Repo { repo });
        self.scheduler.notify_one();
        Ok(())
    }

    pub async fn reset_cache(&self, repo_id: i64, actor: &str) -> Result<()> {
        let repo = self.store.repo(repo_id)?.ok_or_else(|| anyhow!("repository {repo_id} does not exist"))?;
        if self.store.active()?.iter().any(|b| b.repo_id == repo_id) {
            anyhow::bail!("wait until the running build of {} finishes", repo.full_name());
        }
        self.platform.delete_cache(&cache_claim_name(&repo.full_name())).await?;
        self.audit(actor, "cache_reset", Some(repo_id), None, None);
        Ok(())
    }

    async fn scheduler_loop(self: Arc<Self>) {
        let mut tick = tokio::time::interval(Duration::from_secs(10));
        loop {
            tokio::select! {
                _ = tick.tick() => {}
                _ = self.scheduler.notified() => {}
            }
            loop {
                match self.next() {
                    Ok(Some(build)) => {
                        let engine = self.clone();
                        if let Err(error) = engine.dispatch(build).await {
                            tracing::warn!(error = format!("{error:#}"), build, "dispatching the build failed");
                        }
                    }
                    Ok(None) => break,
                    Err(error) => {
                        tracing::warn!(%error, "scheduling failed");
                        break;
                    }
                }
            }
        }
    }

    fn next(&self) -> Result<Option<i64>> {
        let paused: HashSet<i64> = self.store.repos()?.into_iter().filter(|r| r.paused).map(|r| r.id).collect();
        let queued: Vec<schedule::Queued> = self
            .store
            .queued()?
            .into_iter()
            .filter(|b| !paused.contains(&b.repo_id))
            .map(|b| schedule::Queued {
                id: BuildId(b.id),
                repo: RepoId(b.repo_id),
                trigger: core_trigger(&b),
                sha: b.sha.clone(),
                created_at: b.created_at,
                not_before: b.not_before,
            })
            .collect();
        let active: Vec<schedule::Active> = self
            .store
            .active()?
            .into_iter()
            .map(|b| schedule::Active {
                id: BuildId(b.id),
                repo: RepoId(b.repo_id),
                holds_slot: b.state != BuildState::Releasing,
            })
            .collect();
        Ok(schedule::next_dispatch(&queued, &active, self.settings.slots, now()).map(|id| id.0))
    }

    async fn dispatch(self: Arc<Self>, id: i64) -> Result<()> {
        self.dispatching.lock().expect("dispatching set").insert(id);
        let build = self.update(
            id,
            BuildUpdate {
                state: Some(BuildState::Starting),
                started_at: Some(now()),
                phase: Some(Some(Phase::Fetch)),
                ..Default::default()
            },
        );
        let build = match build {
            Ok(build) => build,
            Err(error) => {
                self.dispatching.lock().expect("dispatching set").remove(&id);
                return Err(error);
            }
        };
        let result = self.launch(&build).await;
        self.dispatching.lock().expect("dispatching set").remove(&id);
        match result {
            Ok(()) => Ok(()),
            Err(error) => {
                let detail = format!("{error:#}");
                if self.store.build(id)?.is_some_and(|b| b.state != BuildState::Starting) {
                    return Err(error);
                }
                if let Some(refusal) = error.downcast_ref::<Refusal>() {
                    self.finish(id, BuildState::Failed, Some(refusal.failure), Some(refusal.message.clone()))?;
                } else if build.attempt < self.settings.max_attempts {
                    self.update(
                        id,
                        BuildUpdate {
                            state: Some(BuildState::Queued),
                            attempt: Some(build.attempt + 1),
                            not_before: Some(now() + backoff(self.settings.retry_base_secs, build.attempt)),
                            detail: Some(Some(format!("Retrying after: {detail}"))),
                            ..Default::default()
                        },
                    )?;
                } else {
                    self.finish(id, BuildState::Errored, Some(Failure::Infra), Some(detail))?;
                }
                Err(error)
            }
        }
    }

    async fn launch(&self, build: &Build) -> Result<()> {
        let repo = self.store.repo(build.repo_id)?.context("the repository is gone")?;
        let text = build.config.clone().context("the build has no configuration")?;
        let config = config::parse(&text, &repo.name).map_err(|d| Refusal::config(render_diagnostics(&d)))?;
        let names: Vec<String> = config.images.iter().map(|i| i.name.clone()).collect();
        let pull_request = build.trigger == Trigger::PullRequest;
        if !pull_request && !build.trusted {
            return Err(Refusal::config(format!(
                "{} points at a commit that is not on {}; Kiln only builds and releases tags reachable from the default branch",
                build.ref_name, repo.default_branch
            ))
            .into());
        }
        if !pull_request {
            for image in &config.images {
                if let Some(owner) = self.store.claim_image(&image.name, repo.id)? {
                    let owner =
                        self.store.repo(owner)?.map(|r| r.full_name()).unwrap_or_else(|| "another repository".into());
                    return Err(Refusal::config(format!(
                        "the image {} belongs to {owner}; choose another image name",
                        image.name
                    ))
                    .into());
                }
            }
        }
        if !pull_request {
            self.store.set_images(build.id, &names)?;
        }
        if build.trigger == Trigger::Tag
            && !config.release.shadow
            && let Some((_, images)) = self.store.verified_candidate(repo.id, &build.sha)?
            && images.iter().map(|i| &i.name).eq(names.iter())
        {
            let version = version::tag_version(&build.ref_name).map_err(|e| Refusal::version(e.to_string()))?;
            let mut published = Vec::new();
            for image in &config.images {
                published.extend(self.platform.registry_tags(&self.settings.repository_for(&image.name)).await?);
            }
            version::assess(&version.version, &published).map_err(|e| Refusal::version(e.to_string()))?;
            for image in &images {
                if let Some(digest) = &image.digest {
                    self.store.set_candidate(build.id, &image.name, digest)?;
                }
            }
            self.store.replace_tasks(build.id, build.attempt, &[])?;
            self.update(
                build.id,
                BuildUpdate {
                    state: Some(BuildState::Releasing),
                    phase: Some(Some(Phase::Release)),
                    version: Some(Some(version.version.to_string())),
                    detail: Some(Some("Reusing the image already built and verified for this commit".into())),
                    not_before: Some(now() + self.settings.release_window_secs),
                    ..Default::default()
                },
            )?;
            self.release(build.id).await;
            return Ok(());
        }
        let version = if pull_request { None } else { self.allocate(&repo, build, &config).await? };
        let token = self.platform.fetch_token(&repo).await.context("minting the fetch token")?;
        self.fetch_tokens.lock().expect("fetch tokens").insert((build.id, build.attempt), token.token.clone());
        let started = self.start_pod(&repo, build, &config, version, &token.token).await;
        if started.is_err() {
            self.forget_fetch_token(build.id, build.attempt).await;
        }
        started
    }

    async fn start_pod(
        &self,
        repo: &Repo,
        build: &Build,
        config: &Config,
        version: Option<String>,
        token: &str,
    ) -> Result<()> {
        let pull_request = build.trigger == Trigger::PullRequest;
        let secret = format!("{}-fetch", pod_name(build.id, build.attempt));
        self.platform.create_secret(&secret, build.id, "token", token).await?;
        let claim = cache_claim_name(&repo.full_name());
        self.platform.ensure_cache(&claim).await?;
        let repo_secret = self.settings.repo_secret(&repo.full_name());
        let plan = Plan {
            build_id: build.id,
            attempt: build.attempt,
            repo: &repo.full_name(),
            sha: &build.sha,
            reference: &build.ref_name,
            event: event_name(build),
            version: version.as_deref(),
            run: build.number,
            config,
            scope: if pull_request { Scope::TestsOnly } else { Scope::Full },
            trusted: build.trusted,
            fetch_secret: &secret,
            repo_secret: Some(&repo_secret),
            cache_claim: Some(&claim),
            timeout_secs: self.settings.build_timeout_secs,
        };
        let rendered = kiln_runner::render(&plan, &self.settings.runner).map_err(Refusal::config)?;
        self.store.replace_tasks(build.id, build.attempt, &rendered.tasks)?;
        let name = pod_name(build.id, build.attempt);
        if let Err(error) = self.platform.create_pod(&rendered.pod).await {
            if let Some(rejected) = error.downcast_ref::<PodRejected>() {
                self.forget_fetch_token(build.id, build.attempt).await;
                return Err(Refusal::config(format!("Kubernetes refused the build pod: {rejected}")).into());
            }
            return Err(error);
        }
        let current = self.store.build(build.id)?.context("the build disappeared")?;
        if current.state != BuildState::Starting {
            self.platform.delete_pod(&name).await?;
            self.forget_fetch_token(build.id, build.attempt).await;
            return Ok(());
        }
        self.update(
            build.id,
            BuildUpdate {
                state: Some(BuildState::Running),
                pod: Some(Some(name)),
                version: Some(version),
                detail: if current.cancel_requested { None } else { Some(None) },
                ..Default::default()
            },
        )?;
        if let Some(rc) = self
            .store
            .build(build.id)?
            .and_then(|b| b.version)
            .and_then(|v| v.rsplit_once("-rc.").and_then(|(_, n)| n.parse::<i64>().ok()))
        {
            self.store.raise_high_water(repo.id, rc)?;
        }
        Ok(())
    }

    async fn allocate(&self, repo: &Repo, build: &Build, config: &Config) -> Result<Option<String>> {
        let mut published = Vec::new();
        for image in &config.images {
            published.extend(self.platform.registry_tags(&self.settings.repository_for(&image.name)).await?);
        }
        let allocation = match build.trigger {
            Trigger::Tag => version::tag_version(&build.ref_name).map_err(|e| Refusal::version(e.to_string()))?,
            _ if config.release.mode == ReleaseMode::Tags => return Ok(None),
            _ => {
                let tags = match self.platform.tags(repo, None).await? {
                    Conditional::Fresh { value, .. } => value.into_iter().map(|t| t.name).collect::<Vec<_>>(),
                    Conditional::NotModified => Vec::new(),
                };
                version::next_rc(&tags, &published, repo.rc_high_water as u64)
            }
        };
        match version::assess(&allocation.version, &published) {
            Ok(Publish::Deploys | Publish::BelowCurrent { .. } | Publish::AlreadyPublished) => {
                Ok(Some(allocation.version.to_string()))
            }
            Err(error) => Err(Refusal::version(error.to_string()).into()),
        }
    }

    async fn watch_loop(self: Arc<Self>) {
        let mut events = self.platform.watch();
        while let Some(event) = events.next().await {
            let result = match event {
                PodEvent::Applied(pod) => self.clone().observe_pod(&pod).await,
                PodEvent::Deleted(name) => self.pod_gone(&name).await,
                PodEvent::Restarted => self.recover().await,
            };
            if let Err(error) = result {
                tracing::warn!(error = format!("{error:#}"), "handling a build pod event failed");
            }
        }
    }

    async fn recover(&self) -> Result<()> {
        let pods: HashSet<String> = self.platform.pods().await?.into_iter().filter_map(|p| p.metadata.name).collect();
        for build in self.store.active()? {
            match build.state {
                _ if self.dispatching.lock().expect("dispatching set").contains(&build.id) => {}
                BuildState::Starting if build.pod.is_none() => {
                    self.update(build.id, BuildUpdate { state: Some(BuildState::Queued), ..Default::default() })?;
                }
                BuildState::Running | BuildState::Starting => {
                    if let Some(pod) = &build.pod
                        && !pods.contains(pod)
                    {
                        self.pod_gone(pod).await?;
                    }
                }
                _ => {}
            }
        }
        self.scheduler.notify_one();
        Ok(())
    }

    fn build_for(&self, pod: &Pod) -> Result<Option<(Build, i64)>> {
        let labels = pod.metadata.labels.clone().unwrap_or_default();
        let (Some(build), Some(attempt)) = (
            labels.get(BUILD_LABEL).and_then(|v| v.parse::<i64>().ok()),
            labels.get(ATTEMPT_LABEL).and_then(|v| v.parse::<i64>().ok()),
        ) else {
            return Ok(None);
        };
        Ok(self.store.build(build)?.map(|b| (b, attempt)))
    }

    async fn observe_pod(self: Arc<Self>, pod: &Pod) -> Result<()> {
        let name = pod.metadata.name.clone().unwrap_or_default();
        let Some((build, attempt)) = self.build_for(pod)? else {
            self.platform.delete_pod(&name).await?;
            return Ok(());
        };
        if attempt != build.attempt || build.state.is_finished() || build.state == BuildState::Releasing {
            if pod.metadata.deletion_timestamp.is_none() && self.all_logs_final(&build, attempt)? {
                self.platform.delete_pod(&name).await?;
            }
            return Ok(());
        }
        if build.cancel_requested {
            self.platform.delete_pod(&name).await?;
            return Ok(());
        }
        let tasks = self.store.tasks(build.id, attempt)?;
        let observation = kiln_runner::observe(pod, &tasks, now(), PULL_GRACE_SECS);
        for (ordinal, update) in &observation.tasks {
            self.store.update_task(build.id, attempt, *ordinal, update)?;
        }
        let tasks = self.store.tasks(build.id, attempt)?;
        for task in &tasks {
            match task.state {
                TaskState::Running => self.follow(&build, attempt, task, &name),
                TaskState::Passed | TaskState::Failed if !task.log_final => {
                    self.finalize_log(&build, attempt, task, &name).await?
                }
                _ => {}
            }
            if task.kind == TaskKind::Fetch && task.state != TaskState::Pending && task.state != TaskState::Running {
                self.forget_fetch_token(build.id, attempt).await;
            }
        }
        let phase = tasks
            .iter()
            .find(|t| t.state == TaskState::Running)
            .or_else(|| tasks.iter().rev().find(|t| t.state != TaskState::Pending))
            .map(|t| phase_of(t.kind));
        if phase != build.phase && phase.is_some() {
            self.update(build.id, BuildUpdate { phase: Some(phase), ..Default::default() })?;
        } else {
            let _ = self.logs.events().send(Event::Build { build: build.id, repo: build.repo_id });
        }
        if observation.sidecar_restarts.iter().any(|(name, _)| name == "buildkitd") {
            return self.retry_or_error(&build, &name, "the BuildKit daemon restarted during the build".into()).await;
        }
        match observation.outcome {
            Outcome::Running => Ok(()),
            Outcome::Pending { reason } => {
                let created = pod.metadata.creation_timestamp.as_ref().map(|t| t.0.as_second()).unwrap_or(now());
                if now() - created > PENDING_LIMIT_SECS {
                    let why = reason.unwrap_or_else(|| "it stayed pending".into());
                    return self.retry_or_error(&build, &name, format!("the build could not start: {why}")).await;
                }
                Ok(())
            }
            Outcome::Infra { detail } => self.retry_or_error(&build, &name, detail).await,
            Outcome::Failed { failure, detail } => {
                self.finalize_all_logs(&build, attempt, &name).await?;
                let detail = self.explain(&build, attempt, failure).unwrap_or(detail);
                self.skip_pending(&build, attempt)?;
                self.finish(build.id, BuildState::Failed, Some(failure), Some(detail))?;
                self.platform.delete_pod(&name).await?;
                self.scheduler.notify_one();
                Ok(())
            }
            Outcome::Succeeded => {
                self.finalize_all_logs(&build, attempt, &name).await?;
                if let Some(problem) = self.unexpected_exits(&build, &observation.sidecar_exits)? {
                    self.finish(build.id, BuildState::Failed, Some(Failure::Step), Some(problem))?;
                } else {
                    self.succeeded(&build).await?;
                }
                self.platform.delete_pod(&name).await?;
                self.scheduler.notify_one();
                Ok(())
            }
        }
    }

    fn explain(&self, build: &Build, attempt: i64, failure: Failure) -> Option<String> {
        if !matches!(failure, Failure::Step | Failure::Timeout | Failure::OutOfMemory) {
            return None;
        }
        let tasks = self.store.tasks(build.id, attempt).ok()?;
        let task = tasks.iter().find(|t| matches!(t.state, TaskState::Failed | TaskState::TimedOut))?;
        let job = match task.kind {
            TaskKind::Test => format!("test job {}", task.name),
            TaskKind::Verify => format!("verify job {}", task.name),
            _ => return None,
        };
        let step = task.steps.iter().find(|s| matches!(s.state, TaskState::Failed | TaskState::TimedOut))?;
        Some(match (failure, step.exit_code) {
            (Failure::OutOfMemory, _) => format!("“{}” ran out of memory in {job}; raise resources.memory", step.name),
            (_, Some(124)) => format!("“{}” timed out in {job}", step.name),
            (_, Some(code)) => format!("“{}” failed in {job} (exit code {code})", step.name),
            (_, None) => format!("“{}” failed in {job}", step.name),
        })
    }

    fn unexpected_exits(&self, build: &Build, exits: &[(String, i32)]) -> Result<Option<String>> {
        let Some(text) = &build.config else { return Ok(None) };
        let Ok(config) = config::parse(text, "repository") else { return Ok(None) };
        for (job, spec) in &config.verify {
            for (index, service) in spec.services.iter().enumerate() {
                let container = format!("verify-{job}-svc{index}");
                if let (Some(expected), Some((_, actual))) =
                    (service.expect_exit, exits.iter().find(|(name, _)| *name == container))
                    && *actual != expected
                {
                    return Ok(Some(format!(
                        "service {} in verify job {job} exited with code {actual} when stopped, expected {expected}",
                        service.name
                    )));
                }
            }
        }
        Ok(None)
    }

    async fn succeeded(&self, build: &Build) -> Result<()> {
        let repo = self.store.repo(build.repo_id)?.context("the repository is gone")?;
        let config = build.config.as_deref().and_then(|t| config::parse(t, &repo.name).ok());
        let shadow = config.as_ref().is_some_and(|c| c.release.shadow);
        let tags_only = config.as_ref().is_some_and(|c| c.release.mode == ReleaseMode::Tags);
        if build.trigger == Trigger::PullRequest || build.version.is_none() {
            let detail = (tags_only && build.trigger != Trigger::PullRequest)
                .then(|| "Candidate built and verified; this repository releases on tags".to_owned());
            self.finish(build.id, BuildState::Succeeded, None, detail)?;
            return Ok(());
        }
        if shadow {
            let detail = format!("Shadow run: would release {}", build.version.clone().unwrap_or_default());
            self.finish(build.id, BuildState::Succeeded, None, Some(detail))?;
            return Ok(());
        }
        self.update(
            build.id,
            BuildUpdate {
                state: Some(BuildState::Releasing),
                phase: Some(Some(Phase::Release)),
                not_before: Some(now() + self.settings.release_window_secs),
                ..Default::default()
            },
        )?;
        self.release(build.id).await;
        Ok(())
    }

    pub async fn release(&self, id: i64) {
        if !self.releasing.lock().expect("releasing set").insert(id) {
            return;
        }
        let result = self.promote_all(id).await;
        self.releasing.lock().expect("releasing set").remove(&id);
        if let Err(error) = result {
            let Some(build) = self.store.build(id).ok().flatten().filter(|b| b.state == BuildState::Releasing) else {
                return;
            };
            if build.not_before < build.started_at.unwrap_or(0) {
                let _ = self.update(
                    id,
                    BuildUpdate { not_before: Some(now() + self.settings.release_window_secs), ..Default::default() },
                );
            } else if now() > build.not_before {
                tracing::warn!(error = format!("{error:#}"), build = id, "releasing failed for too long, giving up");
                let _ = self.finish(
                    id,
                    BuildState::Failed,
                    Some(Failure::Release),
                    Some(format!("Releasing kept failing until the release window closed: {error:#}")),
                );
                self.scheduler.notify_one();
            } else {
                tracing::warn!(error = format!("{error:#}"), build = id, "releasing failed, will retry");
                let _ = self.update(
                    id,
                    BuildUpdate { detail: Some(Some(format!("Release is retrying: {error:#}"))), ..Default::default() },
                );
            }
        }
    }

    async fn promote_all(&self, id: i64) -> Result<()> {
        let build = self.store.build(id)?.context("the build is gone")?;
        if build.state != BuildState::Releasing {
            return Ok(());
        }
        let version = build.version.clone().context("the build has no version")?;
        for image in self.store.images(id)? {
            if image.released.is_some() {
                continue;
            }
            let digest = image.digest.clone().with_context(|| format!("{} has no candidate digest", image.name))?;
            let repository = self.settings.repository_for(&image.name);
            match self.platform.promote(&repository, &digest, &version).await {
                Ok(Promotion::Created | Promotion::AlreadyThere) => {
                    self.store.record_release(build.repo_id, &image.name, &version, now())?;
                    self.store.set_released(id, &image.name, &version)?;
                    self.audit(
                        "kiln",
                        "image_released",
                        Some(build.repo_id),
                        Some(id),
                        Some(format!("{repository}:{version} {digest}")),
                    );
                }
                Err(error) => {
                    if let Some(kiln_registry::Error::TagTaken { .. }) = error.downcast_ref::<kiln_registry::Error>() {
                        self.finish(id, BuildState::Failed, Some(Failure::Release), Some(format!("{error:#}")))?;
                        return Ok(());
                    }
                    return Err(error);
                }
            }
        }
        self.finish(id, BuildState::Succeeded, None, Some(format!("Released {version}")))?;
        Ok(())
    }

    async fn retry_or_error(&self, build: &Build, pod: &str, detail: String) -> Result<()> {
        self.finalize_all_logs(build, build.attempt, pod).await?;
        self.platform.delete_pod(pod).await?;
        self.forget_fetch_token(build.id, build.attempt).await;
        if build.attempt < self.settings.max_attempts {
            self.update(
                build.id,
                BuildUpdate {
                    state: Some(BuildState::Queued),
                    attempt: Some(build.attempt + 1),
                    not_before: Some(now() + backoff(self.settings.retry_base_secs, build.attempt)),
                    pod: Some(None),
                    detail: Some(Some(format!("Retrying: {detail}"))),
                    ..Default::default()
                },
            )?;
        } else {
            self.skip_pending(build, build.attempt)?;
            self.finish(build.id, BuildState::Errored, Some(Failure::Infra), Some(detail))?;
        }
        self.scheduler.notify_one();
        Ok(())
    }

    async fn pod_gone(&self, name: &str) -> Result<()> {
        let Some(build) = self.store.active()?.into_iter().find(|b| b.pod.as_deref() == Some(name)) else {
            return Ok(());
        };
        if build.state == BuildState::Releasing {
            return Ok(());
        }
        self.forget_fetch_token(build.id, build.attempt).await;
        if build.cancel_requested {
            self.skip_pending(&build, build.attempt)?;
            let superseded = build.detail.as_deref().is_some_and(|d| d.starts_with("Superseded"));
            let state = if superseded { BuildState::Superseded } else { BuildState::Cancelled };
            self.finish(build.id, state, None, build.detail.clone())?;
            self.scheduler.notify_one();
            return Ok(());
        }
        let detail = "the build pod disappeared, for example because its node was drained".to_owned();
        if build.attempt < self.settings.max_attempts {
            self.update(
                build.id,
                BuildUpdate {
                    state: Some(BuildState::Queued),
                    attempt: Some(build.attempt + 1),
                    not_before: Some(now() + backoff(self.settings.retry_base_secs, build.attempt)),
                    pod: Some(None),
                    detail: Some(Some(format!("Retrying: {detail}"))),
                    ..Default::default()
                },
            )?;
        } else {
            self.finish(build.id, BuildState::Errored, Some(Failure::Infra), Some(detail))?;
        }
        self.scheduler.notify_one();
        Ok(())
    }

    fn skip_pending(&self, build: &Build, attempt: i64) -> Result<()> {
        for task in self.store.tasks(build.id, attempt)? {
            let state = match task.state {
                TaskState::Pending => TaskState::Skipped,
                TaskState::Running => TaskState::Cancelled,
                _ => continue,
            };
            self.store.update_task(
                build.id,
                attempt,
                task.ordinal,
                &TaskUpdate { state: Some(state), finished_at: Some(now()), ..Default::default() },
            )?;
            for step in task.steps.iter().filter(|s| matches!(s.state, TaskState::Pending | TaskState::Running)) {
                let state = if step.state == TaskState::Running { TaskState::Cancelled } else { TaskState::Skipped };
                self.store.update_step(
                    build.id,
                    attempt,
                    task.ordinal,
                    step.ordinal,
                    &StepUpdate {
                        state,
                        exit_code: None,
                        started_at: None,
                        finished_at: Some(now()),
                        log_offset: None,
                    },
                )?;
            }
        }
        Ok(())
    }

    async fn forget_fetch_token(&self, build: i64, attempt: i64) {
        let token = self.fetch_tokens.lock().expect("fetch tokens").remove(&(build, attempt));
        if let Some(token) = token
            && let Err(error) = self.platform.revoke(&token).await
        {
            tracing::warn!(%error, build, "revoking the fetch token failed; it expires within the hour");
        }
        let secret = format!("{}-fetch", pod_name(build, attempt));
        if let Err(error) = self.platform.delete_secret(&secret).await {
            tracing::warn!(%error, secret, "deleting the fetch token secret failed");
        }
    }

    fn all_logs_final(&self, build: &Build, attempt: i64) -> Result<bool> {
        Ok(self
            .store
            .tasks(build.id, attempt)?
            .iter()
            .all(|t| t.log_final || matches!(t.state, TaskState::Pending | TaskState::Skipped)))
    }

    fn follow(self: &Arc<Self>, build: &Build, attempt: i64, task: &Task, pod: &str) {
        let key = (pod.to_owned(), task.container.clone());
        if !self.followed.lock().expect("followers").insert(key.clone()) {
            return;
        }
        let engine = self.clone();
        let (build_id, ordinal, kind, container, pod) =
            (build.id, task.ordinal, task.kind, task.container.clone(), pod.to_owned());
        tokio::spawn(async move {
            match engine.platform.follow(&pod, &container).await {
                Ok(mut stream) => {
                    {
                        let finalized = engine.finalized.lock().expect("finalized");
                        if !finalized.contains(&(build_id, attempt, ordinal))
                            && let Err(error) = engine.logs.reset(build_id, attempt, ordinal)
                        {
                            tracing::warn!(%error, "resetting a live log failed");
                        }
                    }
                    let mut offset = 0i64;
                    let mut pending: Vec<u8> = Vec::new();
                    let mut ended = false;
                    while !ended {
                        match stream.next().await {
                            Some(Ok(bytes)) => pending.extend_from_slice(&bytes),
                            _ => ended = true,
                        }
                        let cut = match pending.iter().rposition(|b| *b == b'\n') {
                            _ if ended || pending.len() >= 64 * 1024 => pending.len(),
                            Some(index) => index + 1,
                            None => 0,
                        };
                        if cut == 0 {
                            continue;
                        }
                        let bytes: Vec<u8> = pending.drain(..cut).collect();
                        let text = String::from_utf8_lossy(&bytes).into_owned();
                        let finalized = engine.finalized.lock().expect("finalized");
                        if finalized.contains(&(build_id, attempt, ordinal)) {
                            break;
                        }
                        for line in text.lines() {
                            if let Some(marker) = parse_marker(line) {
                                engine.apply_marker(build_id, attempt, ordinal, kind, &marker, offset);
                            }
                        }
                        offset += bytes.len() as i64;
                        if let Err(error) = engine.logs.append(build_id, attempt, ordinal, &text) {
                            tracing::warn!(%error, "writing a live log failed");
                        }
                        drop(finalized);
                    }
                }
                Err(error) => tracing::debug!(%error, pod, container, "following the log failed"),
            }
            engine.followed.lock().expect("followers").remove(&key);
        });
    }

    fn apply_marker(&self, build: i64, attempt: i64, task: i64, kind: TaskKind, marker: &Marker, offset: i64) {
        let result = match marker {
            Marker::StepStart { index, at } => self.store.update_step(
                build,
                attempt,
                task,
                *index as i64,
                &StepUpdate {
                    state: TaskState::Running,
                    exit_code: None,
                    started_at: Some(*at),
                    finished_at: None,
                    log_offset: Some(offset),
                },
            ),
            Marker::StepEnd { index, exit, at } => self.store.update_step(
                build,
                attempt,
                task,
                *index as i64,
                &StepUpdate {
                    state: match *exit {
                        0 => TaskState::Passed,
                        124 => TaskState::TimedOut,
                        143 => TaskState::Cancelled,
                        _ => TaskState::Failed,
                    },
                    exit_code: Some(i64::from(*exit)),
                    started_at: None,
                    finished_at: Some(*at),
                    log_offset: None,
                },
            ),
            Marker::Image { name, digest } if kind == TaskKind::Push => self.store.set_candidate(build, name, digest),
            Marker::Image { .. } => Ok(()),
        };
        if let Err(error) = result {
            tracing::warn!(%error, "recording a step marker failed");
        }
    }

    async fn finalize_log(&self, build: &Build, attempt: i64, task: &Task, pod: &str) -> Result<()> {
        if self.finalized.lock().expect("finalized").contains(&(build.id, attempt, task.ordinal)) {
            return Ok(());
        }
        let fetched = self.platform.logs(pod, &task.container).await;
        let live = self.logs.read(build.id, attempt, task.ordinal, 0);
        let text = match fetched {
            Ok(fetched) if fetched.len() >= live.len() => fetched,
            Ok(_) => live.clone(),
            Err(error) => {
                tracing::warn!(
                    error = format!("{error:#}"),
                    pod,
                    container = task.container,
                    "fetching the final log failed; keeping the live copy"
                );
                live.clone()
            }
        };
        let mut offset = 0i64;
        for line in text.split_inclusive('\n') {
            if let Some(marker) = parse_marker(line) {
                self.apply_marker(build.id, attempt, task.ordinal, task.kind, &marker, offset);
            }
            offset += line.len() as i64;
        }
        let bytes = {
            let mut finalized = self.finalized.lock().expect("finalized");
            if !finalized.insert((build.id, attempt, task.ordinal)) {
                return Ok(());
            }
            self.logs.replace(build.id, attempt, task.ordinal, &text)?
        };
        self.store.update_task(
            build.id,
            attempt,
            task.ordinal,
            &TaskUpdate { log_final: Some(true), log_bytes: Some(bytes as i64), ..Default::default() },
        )?;
        Ok(())
    }

    async fn finalize_all_logs(&self, build: &Build, attempt: i64, pod: &str) -> Result<()> {
        for task in self.store.tasks(build.id, attempt)? {
            if !task.log_final && task.state != TaskState::Pending {
                self.finalize_log(build, attempt, &task, pod).await?;
            }
        }
        Ok(())
    }

    async fn outbox_loop(self: Arc<Self>) {
        let mut tick = tokio::time::interval(Duration::from_secs(5));
        loop {
            tokio::select! {
                _ = tick.tick() => {}
                _ = self.outbox.notified() => {}
            }
            if let Err(error) = self.drain_outbox().await {
                tracing::warn!(error = format!("{error:#}"), "updating GitHub checks failed");
            }
        }
    }

    async fn drain_outbox(&self) -> Result<()> {
        let items = self.store.due(now(), 100)?;
        let mut done = HashSet::new();
        for item in items {
            if done.contains(&item.build_id) {
                self.store.outbox_done(item.id, now())?;
                continue;
            }
            match self.send_check(item.build_id).await {
                Ok(()) => {
                    done.insert(item.build_id);
                    self.store.outbox_done(item.id, now())?;
                }
                Err(error) => {
                    let delay = 10 * 2i64.pow(item.attempts.clamp(0, 8) as u32);
                    if item.attempts >= 12 {
                        self.store.outbox_done(item.id, now())?;
                    } else {
                        self.store.outbox_retry(item.id, now() + delay, &format!("{error:#}"))?;
                    }
                    tracing::warn!(error = format!("{error:#}"), build = item.build_id, "GitHub check update failed");
                }
            }
        }
        Ok(())
    }

    async fn send_check(&self, id: i64) -> Result<()> {
        let Some(build) = self.store.build(id)? else { return Ok(()) };
        let repo = self.store.repo(build.repo_id)?.context("the repository is gone")?;
        let tasks = self.store.tasks(build.id, build.attempt)?;
        let images = self.store.images(build.id)?;
        let check = super::checks::check_run(&build, &repo, &tasks, &images, &self.settings.public_url);
        match self.store.check_id(build.id, &check.name)? {
            Some(check_id) => self.platform.update_check(&repo, check_id, &check).await,
            None => {
                let check_id = self.platform.create_check(&repo, &check).await?;
                self.store.set_check_id(build.id, &check.name, check_id)?;
                Ok(())
            }
        }
    }

    async fn housekeeping_loop(self: Arc<Self>) {
        let mut tick = tokio::time::interval(Duration::from_secs(60));
        let mut last_prune = 0;
        loop {
            tick.tick().await;
            let releasing: Vec<i64> = self
                .store
                .active()
                .unwrap_or_default()
                .into_iter()
                .filter(|b| b.state == BuildState::Releasing)
                .map(|b| b.id)
                .collect();
            for id in releasing {
                self.release(id).await;
            }
            if now() - last_prune > 3600 {
                last_prune = now();
                if let Err(error) = self.prune().await {
                    tracing::warn!(error = format!("{error:#}"), "housekeeping failed");
                }
                if let Err(error) = self.scan_registry().await {
                    tracing::warn!(error = format!("{error:#}"), "checking the registry for outside releases failed");
                }
            }
        }
    }

    pub async fn scan_registry(&self) -> Result<()> {
        for repo in self.store.repos()?.into_iter().filter(|r| r.enrolled && r.removed_at.is_none()) {
            if let Err(error) = self.scan_repo_registry(&repo).await {
                tracing::warn!(repo = repo.full_name(), error = format!("{error:#}"), "checking the registry failed");
            }
        }
        Ok(())
    }

    async fn scan_repo_registry(&self, repo: &Repo) -> Result<()> {
        {
            let latest = self.store.builds(&BuildFilter { repo: Some(repo.id), limit: 20, ..Default::default() })?;
            let Some(config) = latest
                .iter()
                .filter(|b| b.trigger != Trigger::PullRequest)
                .find_map(|b| b.config.as_deref().and_then(|text| config::parse(text, &repo.name).ok()))
            else {
                return Ok(());
            };
            let releasing: Vec<String> = self
                .store
                .active()?
                .into_iter()
                .filter(|b| b.repo_id == repo.id && b.state == BuildState::Releasing)
                .filter_map(|b| b.version)
                .collect();
            let mut outside = Vec::new();
            for image in &config.images {
                let tags: Vec<String> = self
                    .platform
                    .registry_tags(&self.settings.repository_for(&image.name))
                    .await?
                    .into_iter()
                    .filter(|tag| semver::Version::parse(tag).is_ok() && !releasing.contains(tag))
                    .collect();
                for tag in self.store.note_registry_tags(repo.id, &image.name, &tags, now())? {
                    outside.push(format!("{}:{tag}", image.name));
                }
            }
            if !outside.is_empty() {
                self.store.append_drift(repo.id, &outside.join(", "))?;
                self.audit("kiln", "registry_drift", Some(repo.id), None, Some(outside.join(", ")));
                let _ = self.logs.events().send(Event::Repo { repo: repo.id });
            }
        }
        Ok(())
    }

    pub fn dismiss_drift(&self, repo: i64, actor: &str) -> Result<()> {
        self.store.set_drift(repo, None)?;
        self.audit(actor, "registry_drift_dismissed", Some(repo), None, None);
        let _ = self.logs.events().send(Event::Repo { repo });
        Ok(())
    }

    async fn prune(&self) -> Result<()> {
        for id in self.store.prune(now(), &Retention::default())? {
            self.logs.remove_build(id);
        }
        let backup = self.settings.data_dir.join("kiln.backup.db");
        self.store.backup(&backup)?;
        let recent = self.store.builds(&BuildFilter { limit: 200, ..Default::default() })?;
        for build in
            recent.iter().filter(|b| b.state.is_finished() && b.finished_at.is_some_and(|t| now() - t < 2 * 86_400))
        {
            for attempt in 1..=build.attempt {
                self.platform.delete_secret(&format!("{}-fetch", pod_name(build.id, attempt))).await?;
            }
        }
        for pod in self.platform.pods().await? {
            if let Some((build, _)) = self.build_for(&pod)?
                && build.state.is_finished()
                && let Some(name) = &pod.metadata.name
            {
                self.platform.delete_pod(name).await?;
            }
        }
        Ok(())
    }
}

#[derive(Debug)]
pub struct Refusal {
    pub failure: Failure,
    pub message: String,
}

impl Refusal {
    fn config(message: impl Into<String>) -> Self {
        Self { failure: Failure::Config, message: message.into() }
    }

    fn version(message: impl Into<String>) -> Self {
        Self { failure: Failure::Version, message: message.into() }
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Refusal {}

fn core_trigger(build: &Build) -> schedule::Trigger {
    match build.trigger {
        Trigger::Tag => schedule::Trigger::Tag(build.ref_name.clone()),
        Trigger::Push => schedule::Trigger::DefaultBranch,
        Trigger::Manual => schedule::Trigger::Manual,
        Trigger::PullRequest => schedule::Trigger::PullRequest(build.pr_number.unwrap_or_default() as u64),
    }
}

fn event_name(build: &Build) -> &'static str {
    match build.trigger {
        Trigger::Push => "push",
        Trigger::Tag => "tag",
        Trigger::PullRequest => "pull_request",
        Trigger::Manual => "manual",
    }
}

fn phase_of(kind: TaskKind) -> Phase {
    match kind {
        TaskKind::Fetch => Phase::Fetch,
        TaskKind::Test => Phase::Test,
        TaskKind::Image | TaskKind::Push => Phase::Build,
        TaskKind::Verify => Phase::Verify,
    }
}

pub fn render_diagnostics(diagnostics: &[config::Diagnostic]) -> String {
    diagnostics.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n")
}
