use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use futures::StreamExt;
use futures::stream::BoxStream;
use kiln_github::{CheckRun, Conditional, Head, PullRequest, TagRef, Token, unix_now};
use kiln_registry::Promotion;
use kiln_runner::k8s_openapi::api::core::v1::Pod;
use kiln_runner::{Marker, StepSpec, render_marker};
use kiln_store::Repo;
use serde_json::{Value, json};
use tokio::sync::broadcast;

use super::platform::{Platform, PodEvent, RepoInfo};

#[derive(Debug, Clone)]
pub struct Pace {
    pub step: Duration,
    pub line: Duration,
}

impl Pace {
    pub fn instant() -> Self {
        Self { step: Duration::from_millis(5), line: Duration::ZERO }
    }

    pub fn demo() -> Self {
        Self { step: Duration::from_millis(1500), line: Duration::from_millis(120) }
    }
}

#[derive(Default)]
struct State {
    repos: Vec<RepoInfo>,
    heads: HashMap<i64, Head>,
    files: HashMap<(i64, String), String>,
    tags: HashMap<i64, Vec<(String, String)>>,
    pulls: HashMap<i64, Vec<PullRequest>>,
    registry: HashMap<String, HashMap<String, String>>,
    pods: HashMap<String, Pod>,
    logs: HashMap<(String, String), String>,
    checks: HashMap<i64, CheckRun>,
    next_check: i64,
    secrets: HashMap<String, i64>,
    caches: HashSet<String>,
    revoked: Vec<String>,
    failing: HashSet<String>,
    lost: HashSet<String>,
    generation: u64,
}

pub struct Fake {
    state: Arc<Mutex<State>>,
    events: broadcast::Sender<PodEvent>,
    pace: Pace,
}

impl Fake {
    pub fn new(pace: Pace) -> Self {
        let (events, _) = broadcast::channel(1024);
        Self { state: Arc::new(Mutex::new(State::default())), events, pace }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().expect("fake state")
    }

    pub fn add_repo(&self, id: i64, owner: &str, name: &str) {
        self.state().repos.push(RepoInfo {
            id,
            installation_id: 1,
            owner: owner.into(),
            name: name.into(),
            default_branch: "main".into(),
            private: true,
            archived: false,
        });
    }

    pub fn push(&self, repo: i64, sha: &str, message: &str, config: Option<&str>) {
        let mut state = self.state();
        state.heads.insert(repo, Head { sha: sha.into(), message: message.into() });
        if let Some(config) = config {
            state.files.insert((repo, sha.into()), config.into());
        }
        state.generation += 1;
    }

    pub fn tag(&self, repo: i64, name: &str, sha: &str) {
        let mut state = self.state();
        state.tags.entry(repo).or_default().push((name.into(), sha.into()));
        state.generation += 1;
    }

    pub fn open_pull(&self, repo: i64, number: u64, sha: &str, title: &str, config: &str) {
        let mut state = self.state();
        state.files.insert((repo, sha.into()), config.into());
        let pulls = state.pulls.entry(repo).or_default();
        pulls.retain(|p| p.number != number);
        pulls.push(PullRequest {
            number,
            head_sha: sha.into(),
            head_ref: format!("feature-{number}"),
            base_ref: "main".into(),
            from_fork: false,
            draft: false,
            title: title.into(),
        });
        state.generation += 1;
    }

    pub fn publish(&self, repository: &str, tag: &str, digest: &str) {
        self.state().registry.entry(repository.into()).or_default().insert(tag.into(), digest.into());
    }

    pub fn fail_container(&self, container: &str) {
        self.state().failing.insert(container.into());
    }

    pub fn lose_next_pod(&self, container: &str) {
        self.state().lost.insert(container.into());
    }

    pub fn registry_tag(&self, repository: &str, tag: &str) -> Option<String> {
        self.state().registry.get(repository).and_then(|tags| tags.get(tag).cloned())
    }

    pub fn checks(&self) -> Vec<CheckRun> {
        let state = self.state();
        let mut checks: Vec<(i64, CheckRun)> = state.checks.iter().map(|(k, v)| (*k, v.clone())).collect();
        checks.sort_by_key(|(id, _)| *id);
        checks.into_iter().map(|(_, c)| c).collect()
    }

    pub fn pod_names(&self) -> Vec<String> {
        self.state().pods.keys().cloned().collect()
    }

    pub fn pod(&self, name: &str) -> Option<Pod> {
        self.state().pods.get(name).cloned()
    }

    pub fn revoked(&self) -> usize {
        self.state().revoked.len()
    }

    fn etag(&self) -> String {
        format!("\"{}\"", self.state().generation)
    }

    fn conditional<T>(&self, etag: Option<&str>, value: T) -> Conditional<T> {
        let current = self.etag();
        if etag == Some(current.as_str()) {
            Conditional::NotModified
        } else {
            Conditional::Fresh { value, etag: Some(current) }
        }
    }

    fn emit(&self, name: &str) {
        if let Some(pod) = self.state().pods.get(name).cloned() {
            let _ = self.events.send(PodEvent::Applied(Box::new(pod)));
        }
    }

    fn set_status(&self, name: &str, status: Value) {
        let mut state = self.state();
        if let Some(pod) = state.pods.get_mut(name) {
            let mut value = serde_json::to_value(&*pod).expect("pod serializes");
            value["status"] = status;
            *pod = serde_json::from_value(value).expect("pod deserializes");
        }
    }

    fn append_log(&self, pod: &str, container: &str, line: &str) {
        let mut state = self.state();
        let log = state.logs.entry((pod.into(), container.into())).or_default();
        log.push_str(line);
        log.push('\n');
    }

    fn exists(&self, pod: &str) -> bool {
        self.state().pods.contains_key(pod)
    }

    async fn simulate(self: Arc<Self>, name: String) {
        let Some(pod) = self.pod(&name) else { return };
        let spec = serde_json::to_value(pod.spec.clone().unwrap_or_default()).unwrap_or_default();
        let containers: Vec<Value> = spec["initContainers"].as_array().cloned().unwrap_or_default();
        let created = time(unix_now());
        let mut statuses: Vec<Value> = containers.iter().map(|c| waiting(c)).collect();
        self.set_status(&name, json!({ "phase": "Pending", "initContainerStatuses": statuses }));
        self.emit(&name);
        tokio::time::sleep(self.pace.step).await;
        for (index, container) in containers.iter().enumerate() {
            if !self.exists(&name) {
                return;
            }
            let container_name = container["name"].as_str().unwrap_or_default().to_owned();
            let sidecar = container["restartPolicy"].as_str() == Some("Always");
            statuses[index] = running(container);
            self.set_status(
                &name,
                json!({ "phase": "Running", "initContainerStatuses": statuses, "startTime": created }),
            );
            self.emit(&name);
            if sidecar {
                continue;
            }
            let lost = self.state().lost.remove(&container_name);
            if lost {
                self.state().pods.remove(&name);
                let _ = self.events.send(PodEvent::Deleted(name.clone()));
                return;
            }
            let exit = self.run_container(&name, container).await;
            if !self.exists(&name) {
                return;
            }
            statuses[index] = terminated(container, exit);
            let phase = if exit == 0 { "Running" } else { "Failed" };
            self.set_status(&name, json!({ "phase": phase, "initContainerStatuses": statuses, "startTime": created }));
            self.emit(&name);
            if exit != 0 {
                return;
            }
        }
        for (index, container) in containers.iter().enumerate() {
            if container["restartPolicy"].as_str() == Some("Always") {
                statuses[index] = terminated(container, 0);
            }
        }
        self.set_status(
            &name,
            json!({
                "phase": "Succeeded",
                "initContainerStatuses": statuses,
                "containerStatuses": [terminated(&json!({ "name": "done", "image": "kiln" }), 0)],
                "startTime": created,
            }),
        );
        self.emit(&name);
    }

    async fn run_container(&self, pod: &str, container: &Value) -> i32 {
        let name = container["name"].as_str().unwrap_or_default().to_owned();
        let fails = self.state().failing.contains(&name);
        let args: Vec<String> = container["args"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect())
            .unwrap_or_default();
        let env = |key: &str| {
            container["env"]
                .as_array()
                .and_then(|env| env.iter().find(|e| e["name"] == key))
                .and_then(|e| e["value"].as_str())
                .map(str::to_owned)
        };
        if name == "fetch" {
            let repo = args.iter().skip_while(|a| *a != "--repo").nth(1).cloned().unwrap_or_default();
            let sha = args.iter().skip_while(|a| *a != "--sha").nth(1).cloned().unwrap_or_default();
            self.say(pod, &name, &format!("Fetching {repo} at {sha}")).await;
            self.say(pod, &name, "Fetched 412 files (3.8 MB) in 0.9s").await;
            return 0;
        }
        if let Some(steps) = env("KILN_STEPS") {
            let steps: Vec<StepSpec> = serde_json::from_str::<Vec<Value>>(&steps)
                .unwrap_or_default()
                .into_iter()
                .map(|s| StepSpec {
                    name: s["name"].as_str().unwrap_or_default().into(),
                    run: s["run"].as_str().unwrap_or_default().into(),
                })
                .collect();
            let last = steps.len().saturating_sub(1);
            for (index, step) in steps.iter().enumerate() {
                self.say(pod, &name, &render_marker(&Marker::StepStart { index, at: unix_now() })).await;
                for line in step.run.lines() {
                    self.say(pod, &name, &format!("$ {line}")).await;
                }
                for line in output_for(&step.run) {
                    self.say(pod, &name, line).await;
                }
                tokio::time::sleep(self.pace.step).await;
                let exit = if fails && index == last { 1 } else { 0 };
                if exit != 0 {
                    self.say(pod, &name, "Error: 1 test failed: renders the checkout page (expected 200, got 500)")
                        .await;
                }
                self.say(pod, &name, &render_marker(&Marker::StepEnd { index, exit, at: unix_now() })).await;
                if exit != 0 {
                    return exit;
                }
            }
            return 0;
        }
        if name.starts_with("image-") {
            for line in [
                "#1 [internal] load build definition from Dockerfile",
                "#2 [internal] load metadata for docker.io/library/node:26-slim",
                "#3 [deps 1/3] COPY package.json package-lock.json ./",
                "#4 [deps 2/3] RUN npm ci",
                "#4 CACHED",
                "#5 [builder 3/4] RUN npm run build",
                "#5 12.31 ✓ Compiled successfully",
                "#6 exporting to oci image format",
                "#6 DONE 2.4s",
            ] {
                self.say(pod, &name, line).await;
            }
            tokio::time::sleep(self.pace.step).await;
            return if fails { 1 } else { 0 };
        }
        if name == "push" {
            let tag = args.iter().skip_while(|a| *a != "--tag").nth(1).cloned().unwrap_or_default();
            let images: Vec<String> = args
                .iter()
                .enumerate()
                .filter(|(i, _)| *i > 0 && args[i - 1] == "--image")
                .map(|(_, v)| v.clone())
                .collect();
            for image in images {
                let mut parts = image.splitn(3, '=');
                let (Some(image_name), Some(repository)) = (parts.next(), parts.next()) else { continue };
                let digest = format!(
                    "sha256:{:064x}",
                    (unix_now() as u64).wrapping_mul(2654435761).wrapping_add(image_name.len() as u64)
                );
                self.say(pod, &name, &format!("Pushing {image_name} to ghcr.io/{repository}:{tag}")).await;
                self.publish(repository, &tag, &digest);
                self.say(pod, &name, &format!("Pushed {image_name} as {digest} in 3.2s")).await;
                self.say(pod, &name, &render_marker(&Marker::Image { name: image_name.into(), digest })).await;
            }
            return 0;
        }
        0
    }

    async fn say(&self, pod: &str, container: &str, line: &str) {
        self.append_log(pod, container, line);
        if !self.pace.line.is_zero() {
            tokio::time::sleep(self.pace.line).await;
        }
    }
}

fn output_for(run: &str) -> Vec<&'static str> {
    if run.contains("npm ci") {
        vec!["added 812 packages, and audited 813 packages in 14s", "found 0 vulnerabilities"]
    } else if run.contains("lint") {
        vec!["> eslint .", "✔ No problems found"]
    } else if run.contains("typecheck") {
        vec!["> tsc --noEmit"]
    } else if run.contains("test") {
        vec![
            " ✓ src/lib/cart.test.ts (14 tests) 41ms",
            " ✓ src/lib/auth.test.ts (9 tests) 28ms",
            " Test Files  2 passed (2)",
            "      Tests  23 passed (23)",
        ]
    } else if run.contains("curl") {
        vec!["{\"status\":\"ok\"}"]
    } else {
        vec![]
    }
}

fn time(seconds: i64) -> String {
    kiln_runner::k8s_openapi::jiff::Timestamp::from_second(seconds).map(|t| t.to_string()).unwrap_or_default()
}

fn base(container: &Value) -> Value {
    json!({ "name": container["name"], "image": container["image"], "imageID": "", "ready": false, "restartCount": 0 })
}

fn waiting(container: &Value) -> Value {
    let mut status = base(container);
    status["state"] = json!({ "waiting": { "reason": "PodInitializing" } });
    status
}

fn running(container: &Value) -> Value {
    let mut status = base(container);
    status["state"] = json!({ "running": { "startedAt": time(unix_now()) } });
    status
}

fn terminated(container: &Value, exit: i32) -> Value {
    let mut status = base(container);
    let reason = if exit == 0 { "Completed" } else { "Error" };
    status["state"] = json!({ "terminated": { "exitCode": exit, "reason": reason, "startedAt": time(unix_now() - 1), "finishedAt": time(unix_now()) } });
    status
}

impl Platform for Arc<Fake> {
    async fn repositories(&self) -> Result<Vec<RepoInfo>> {
        Ok(self.state().repos.clone())
    }

    async fn head(&self, repo: &Repo, etag: Option<&str>) -> Result<Conditional<Head>> {
        let head =
            self.state().heads.get(&repo.id).cloned().ok_or_else(|| anyhow!("{} has no commits", repo.full_name()))?;
        Ok(self.conditional(etag, head))
    }

    async fn tags(&self, repo: &Repo, etag: Option<&str>) -> Result<Conditional<Vec<TagRef>>> {
        let tags = self.state().tags.get(&repo.id).cloned().unwrap_or_default();
        let refs = tags.into_iter().map(|(name, sha)| TagRef { name, object_sha: sha, annotated: false }).collect();
        Ok(self.conditional(etag, refs))
    }

    async fn tag_commit(&self, _repo: &Repo, tag: &TagRef) -> Result<String> {
        Ok(tag.object_sha.clone())
    }

    async fn pulls(&self, repo: &Repo, etag: Option<&str>) -> Result<Conditional<Vec<PullRequest>>> {
        let pulls = self.state().pulls.get(&repo.id).cloned().unwrap_or_default();
        Ok(self.conditional(etag, pulls))
    }

    async fn file(&self, repo: &Repo, path: &str, sha: &str) -> Result<Option<String>> {
        if path != kiln_core::config::FILE_NAME {
            return Ok(None);
        }
        Ok(self.state().files.get(&(repo.id, sha.to_owned())).cloned())
    }

    async fn commit_message(&self, repo: &Repo, sha: &str) -> Result<String> {
        let state = self.state();
        Ok(state
            .heads
            .get(&repo.id)
            .filter(|h| h.sha == sha)
            .map(|h| h.message.clone())
            .unwrap_or_else(|| "Release".into()))
    }

    async fn on_default_branch(&self, _repo: &Repo, _sha: &str) -> Result<bool> {
        Ok(true)
    }

    async fn fetch_token(&self, _repo: &Repo) -> Result<Token> {
        Ok(Token { token: format!("ghs_fake{}", unix_now()), expires_at: unix_now() + 3600 })
    }

    async fn revoke(&self, token: &str) -> Result<()> {
        self.state().revoked.push(token.into());
        Ok(())
    }

    async fn create_check(&self, _repo: &Repo, check: &CheckRun) -> Result<i64> {
        let mut state = self.state();
        state.next_check += 1;
        let id = state.next_check;
        state.checks.insert(id, check.clone());
        Ok(id)
    }

    async fn update_check(&self, _repo: &Repo, id: i64, check: &CheckRun) -> Result<()> {
        self.state().checks.insert(id, check.clone());
        Ok(())
    }

    async fn registry_tags(&self, repository: &str) -> Result<Vec<String>> {
        Ok(self.state().registry.get(repository).map(|tags| tags.keys().cloned().collect()).unwrap_or_default())
    }

    async fn promote(&self, repository: &str, digest: &str, tag: &str) -> Result<Promotion> {
        let mut state = self.state();
        let tags = state.registry.entry(repository.into()).or_default();
        match tags.get(tag) {
            Some(existing) if existing == digest => Ok(Promotion::AlreadyThere),
            Some(existing) => Err(kiln_registry::Error::TagTaken {
                repository: repository.into(),
                tag: tag.into(),
                existing: existing.clone(),
            }
            .into()),
            None => {
                if !tags.values().any(|d| d == digest) {
                    bail!("the candidate {digest} is not in {repository}");
                }
                tags.insert(tag.into(), digest.into());
                Ok(Promotion::Created)
            }
        }
    }

    async fn ensure_cache(&self, claim: &str) -> Result<()> {
        self.state().caches.insert(claim.into());
        Ok(())
    }

    async fn delete_cache(&self, claim: &str) -> Result<()> {
        self.state().caches.remove(claim);
        Ok(())
    }

    async fn create_secret(&self, name: &str, build: i64, _key: &str, _value: &str) -> Result<()> {
        self.state().secrets.insert(name.into(), build);
        Ok(())
    }

    async fn delete_secret(&self, name: &str) -> Result<()> {
        self.state().secrets.remove(name);
        Ok(())
    }

    async fn build_secrets(&self) -> Result<Vec<(String, i64)>> {
        Ok(self.state().secrets.iter().map(|(k, v)| (k.clone(), *v)).collect())
    }

    async fn create_pod(&self, pod: &Pod) -> Result<()> {
        let name = pod.metadata.name.clone().unwrap_or_default();
        let mut stored = pod.clone();
        stored.metadata.creation_timestamp = Some(kiln_runner::k8s_openapi::apimachinery::pkg::apis::meta::v1::Time(
            kiln_runner::k8s_openapi::jiff::Timestamp::from_second(unix_now()).expect("valid time"),
        ));
        {
            let mut state = self.state();
            if state.pods.contains_key(&name) {
                return Ok(());
            }
            state.pods.insert(name.clone(), stored);
        }
        tokio::spawn(self.clone().simulate(name));
        Ok(())
    }

    async fn delete_pod(&self, name: &str) -> Result<()> {
        if self.state().pods.remove(name).is_some() {
            let _ = self.events.send(PodEvent::Deleted(name.into()));
        }
        Ok(())
    }

    async fn pods(&self) -> Result<Vec<Pod>> {
        Ok(self.state().pods.values().cloned().collect())
    }

    fn watch(&self) -> BoxStream<'static, PodEvent> {
        let receiver = self.events.subscribe();
        futures::stream::once(async { PodEvent::Restarted })
            .chain(tokio_stream::wrappers::BroadcastStream::new(receiver).filter_map(|e| async move { e.ok() }))
            .boxed()
    }

    async fn logs(&self, pod: &str, container: &str) -> Result<String> {
        Ok(self.state().logs.get(&(pod.into(), container.into())).cloned().unwrap_or_default())
    }

    async fn follow(&self, pod: &str, container: &str) -> Result<BoxStream<'static, Result<Vec<u8>>>> {
        let state = self.state.clone();
        let key = (pod.to_owned(), container.to_owned());
        let stream = futures::stream::unfold((0usize, 0u32), move |(sent, idle)| {
            let state = state.clone();
            let key = key.clone();
            async move {
                loop {
                    let (log, alive) = {
                        let guard = state.lock().expect("fake state");
                        let log = guard.logs.get(&key).cloned().unwrap_or_default();
                        let alive = guard.pods.get(&key.0).is_some_and(|pod| {
                            let status =
                                serde_json::to_value(pod.status.clone().unwrap_or_default()).unwrap_or_default();
                            status["initContainerStatuses"].as_array().is_some_and(|all| {
                                all.iter().any(|c| c["name"] == key.1.as_str() && c["state"].get("running").is_some())
                            })
                        });
                        (log, alive)
                    };
                    if log.len() > sent {
                        let chunk = log[sent..].as_bytes().to_vec();
                        return Some((Ok(chunk), (log.len(), 0)));
                    }
                    if !alive && idle > 3 {
                        return None;
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    let _ = idle;
                    return Some((Ok(Vec::new()), (sent, idle + 1)));
                }
            }
        })
        .filter(|chunk| futures::future::ready(!matches!(chunk, Ok(bytes) if bytes.is_empty())));
        Ok(stream.boxed())
    }
}
