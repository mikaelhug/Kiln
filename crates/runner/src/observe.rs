use k8s_openapi::api::core::v1::{ContainerStatus, Pod};
use kiln_store::{Failure, Task, TaskKind, TaskState, TaskUpdate};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Pending { reason: Option<String> },
    Running,
    Succeeded,
    Failed { failure: Failure, detail: String },
    Infra { detail: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    pub outcome: Outcome,
    pub tasks: Vec<(i64, TaskUpdate)>,
    pub sidecar_exits: Vec<(String, i32)>,
    pub sidecar_restarts: Vec<(String, i32)>,
}

pub fn observe(pod: &Pod, tasks: &[Task], now: i64, pull_grace: i64) -> Observation {
    let status = pod.status.clone().unwrap_or_default();
    let mut statuses: Vec<ContainerStatus> = status.init_container_statuses.clone().unwrap_or_default();
    statuses.extend(status.container_statuses.clone().unwrap_or_default());
    let mut updates = Vec::new();
    let mut config_error = None;
    let mut infra_error = None;
    let mut first_failure: Option<(Failure, String)> = None;
    let mut sidecar_exits = Vec::new();
    let mut sidecar_restarts = Vec::new();
    let mut service_failure = None;

    let since = pod.metadata.creation_timestamp.as_ref().map(|t| t.0.as_second()).unwrap_or(now);
    for container in &statuses {
        let task = tasks.iter().find(|t| t.container == container.name);
        let state = container.state.clone().unwrap_or_default();
        if task.is_none() {
            if let Some(terminated) = &state.terminated {
                sidecar_exits.push((container.name.clone(), terminated.exit_code));
            }
            if container.restart_count > 0 {
                sidecar_restarts.push((container.name.clone(), container.restart_count));
                if container.name != "buildkitd" && service_failure.is_none() {
                    service_failure = Some(format!("the service in {} stopped or never started", container.name));
                }
            }
            if let Some(waiting) = &state.waiting {
                let pulling = matches!(waiting.reason.as_deref(), Some("ImagePullBackOff" | "ErrImagePull"));
                if !(pulling && now - since < pull_grace) {
                    let label = if container.name == "buildkitd" {
                        "BuildKit".to_owned()
                    } else {
                        format!("the service in {}", container.name)
                    };
                    classify_waiting(
                        &label,
                        waiting.reason.as_deref(),
                        waiting.message.as_deref(),
                        &mut config_error,
                        &mut infra_error,
                        container.image.as_str(),
                        container.name == "buildkitd",
                    );
                }
            }
            continue;
        }
        let task = task.expect("checked above");
        let mut update = TaskUpdate::default();
        if let Some(running) = &state.running {
            update.state = Some(TaskState::Running);
            update.started_at = running.started_at.as_ref().map(|t| t.0.as_second()).filter(|t| *t > 0);
        }
        if let Some(terminated) = &state.terminated {
            update.started_at = terminated.started_at.as_ref().map(|t| t.0.as_second()).filter(|t| *t > 0);
            update.finished_at = terminated.finished_at.as_ref().map(|t| t.0.as_second()).or(Some(now));
            update.exit_code = Some(i64::from(terminated.exit_code));
            update.reason = terminated.reason.clone();
            let oom = terminated.reason.as_deref() == Some("OOMKilled");
            let unstartable = matches!(terminated.reason.as_deref(), Some("StartError" | "ContainerCannotRun"));
            update.state = Some(if terminated.exit_code == 0 { TaskState::Passed } else { TaskState::Failed });
            if terminated.exit_code != 0 && first_failure.is_none() {
                first_failure = Some(if unstartable {
                    let message = terminated.message.as_deref().unwrap_or_default();
                    let why: String = ["container init: ", "runc create failed: ", "OCI runtime create failed: "]
                        .iter()
                        .find_map(|marker| message.split_once(marker).map(|(_, rest)| rest))
                        .unwrap_or(message)
                        .trim()
                        .chars()
                        .take(300)
                        .collect();
                    let failure = if matches!(task.kind, TaskKind::Test | TaskKind::Verify) {
                        Failure::Config
                    } else {
                        Failure::Setup
                    };
                    (failure, format!("{} could not start its container: {why}", task_label(task)))
                } else if oom {
                    (
                        Failure::OutOfMemory,
                        format!("{} ran out of memory; raise its resources.memory", task_label(task)),
                    )
                } else {
                    (task_failure(task.kind), format!("{} exited with code {}", task_label(task), terminated.exit_code))
                });
            }
        }
        if let Some(waiting) = &state.waiting {
            let user_image = matches!(task.kind, TaskKind::Test | TaskKind::Verify);
            let patient = now - since < pull_grace;
            let reason = waiting.reason.as_deref();
            if !(patient && matches!(reason, Some("ImagePullBackOff" | "ErrImagePull"))) {
                classify_waiting(
                    &task_label(task),
                    reason,
                    waiting.message.as_deref(),
                    &mut config_error,
                    &mut infra_error,
                    container.image.as_str(),
                    !user_image,
                );
            }
        }
        if update != TaskUpdate::default() {
            updates.push((task.ordinal, update));
        }
    }

    let phase = status.phase.as_deref().unwrap_or("Pending");
    let reason = status.reason.clone().unwrap_or_default();
    let message = status.message.clone().unwrap_or_default();
    let outcome = if let Some(detail) = config_error {
        Outcome::Failed { failure: Failure::Config, detail }
    } else if let Some(detail) = service_failure {
        Outcome::Failed { failure: Failure::Step, detail }
    } else if reason == "DeadlineExceeded" {
        Outcome::Failed { failure: Failure::Timeout, detail: "the build ran past its timeout".into() }
    } else if reason == "Evicted" {
        if message.contains("ephemeral") || message.contains("emptyDir") {
            Outcome::Failed { failure: Failure::Storage, detail: format!("out of disk space: {message}") }
        } else {
            Outcome::Infra { detail: format!("the node evicted the build: {message}") }
        }
    } else if let Some(detail) = infra_error {
        Outcome::Infra { detail }
    } else {
        match phase {
            "Succeeded" => Outcome::Succeeded,
            "Failed" => match first_failure {
                Some((failure, detail)) => Outcome::Failed { failure, detail },
                None => {
                    Outcome::Infra { detail: format!("the build pod failed: {} {}", reason, message).trim().to_owned() }
                }
            },
            "Running" => match first_failure {
                Some((failure, detail)) => Outcome::Failed { failure, detail },
                None => Outcome::Running,
            },
            _ => Outcome::Pending { reason: pending_reason(pod) },
        }
    };
    Observation { outcome, tasks: updates, sidecar_exits, sidecar_restarts }
}

fn task_label(task: &Task) -> String {
    match task.kind {
        TaskKind::Fetch => "fetching the source".into(),
        TaskKind::Test => format!("test job {}", task.name),
        TaskKind::Verify => format!("verify job {}", task.name),
        TaskKind::Image => format!("the {} image build", task.name),
        TaskKind::Push => "pushing the candidate".into(),
    }
}

fn task_failure(kind: TaskKind) -> Failure {
    match kind {
        TaskKind::Fetch | TaskKind::Push => Failure::Infra,
        TaskKind::Image => Failure::Image,
        TaskKind::Test | TaskKind::Verify => Failure::Step,
    }
}

fn classify_waiting(
    what: &str,
    reason: Option<&str>,
    message: Option<&str>,
    config_error: &mut Option<String>,
    infra_error: &mut Option<String>,
    image: &str,
    ours: bool,
) {
    let message = message.unwrap_or_default();
    match reason {
        Some("CreateContainerConfigError") => {
            *config_error = Some(format!("{what} cannot start: {message}"));
        }
        Some("InvalidImageName") => {
            *config_error = Some(format!("{what} uses an invalid image name {image:?}"));
        }
        Some("ImagePullBackOff" | "ErrImagePull") if ours => {
            *infra_error = Some(format!("pulling {image} failed: {message}"));
        }
        Some("ImagePullBackOff" | "ErrImagePull") => {
            *config_error = Some(format!("{what} uses an image that cannot be pulled: {image}"));
        }
        _ => {}
    }
}

fn pending_reason(pod: &Pod) -> Option<String> {
    let conditions = pod.status.as_ref()?.conditions.as_ref()?;
    conditions
        .iter()
        .find(|c| c.type_ == "PodScheduled" && c.status == "False")
        .map(|c| c.message.clone().unwrap_or_else(|| c.reason.clone().unwrap_or_default()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn task(ordinal: i64, kind: TaskKind, container: &str) -> Task {
        Task {
            ordinal,
            kind,
            name: container.into(),
            container: container.into(),
            state: TaskState::Pending,
            exit_code: None,
            reason: None,
            started_at: None,
            finished_at: None,
            log_final: false,
            log_bytes: None,
            steps: Vec::new(),
        }
    }

    fn pod(status: serde_json::Value) -> Pod {
        serde_json::from_value(json!({
            "metadata": { "name": "kiln-b1-a1", "creationTimestamp": "2026-01-01T00:00:00Z" },
            "status": status,
        }))
        .unwrap()
    }

    fn terminated(name: &str, code: i32, reason: &str) -> serde_json::Value {
        json!({ "name": name, "image": "x", "imageID": "", "ready": false, "restartCount": 0,
                "state": { "terminated": { "exitCode": code, "reason": reason,
                  "startedAt": "2026-01-01T00:00:10Z", "finishedAt": "2026-01-01T00:01:00Z" } } })
    }

    fn waiting(name: &str, image: &str, reason: &str) -> serde_json::Value {
        json!({ "name": name, "image": image, "imageID": "", "ready": false, "restartCount": 0,
                "state": { "waiting": { "reason": reason, "message": "boom" } } })
    }

    #[test]
    fn step_failures_and_oom_are_build_failures_not_retries() {
        let tasks = [task(0, TaskKind::Fetch, "fetch"), task(1, TaskKind::Test, "test-unit")];
        let failed = pod(json!({ "phase": "Failed", "initContainerStatuses": [
            terminated("fetch", 0, "Completed"), terminated("test-unit", 2, "Error")] }));
        let seen = observe(&failed, &tasks, 1_767_225_700, 120);
        assert_eq!(
            seen.outcome,
            Outcome::Failed { failure: Failure::Step, detail: "test job test-unit exited with code 2".into() }
        );
        assert_eq!(seen.tasks[1].1.state, Some(TaskState::Failed));
        let oom = pod(json!({ "phase": "Failed", "initContainerStatuses": [
            terminated("fetch", 0, "Completed"), terminated("test-unit", 137, "OOMKilled")] }));
        assert!(matches!(observe(&oom, &tasks, 0, 120).outcome, Outcome::Failed { failure: Failure::OutOfMemory, .. }));
    }

    #[test]
    fn missing_images_and_secrets_are_config_errors_but_our_images_retry() {
        let tasks = [task(0, TaskKind::Fetch, "fetch"), task(1, TaskKind::Test, "test-unit")];
        let created = 1_767_225_600;
        let user = pod(json!({ "phase": "Pending", "initContainerStatuses": [
            terminated("fetch", 0, "Completed"), waiting("test-unit", "nod:26", "ImagePullBackOff")] }));
        assert!(matches!(observe(&user, &tasks, created + 30, 120).outcome, Outcome::Pending { .. }));
        assert!(matches!(
            observe(&user, &tasks, created + 300, 120).outcome,
            Outcome::Failed { failure: Failure::Config, .. }
        ));
        let ours =
            pod(json!({ "phase": "Pending", "initContainerStatuses": [waiting("fetch", "kiln", "ErrImagePull")] }));
        assert!(matches!(observe(&ours, &tasks, created + 300, 120).outcome, Outcome::Infra { .. }));
        let secret = pod(json!({ "phase": "Pending", "initContainerStatuses": [
            terminated("fetch", 0, "Completed"), waiting("test-unit", "node:26", "CreateContainerConfigError")] }));
        assert!(matches!(
            observe(&secret, &tasks, created, 120).outcome,
            Outcome::Failed { failure: Failure::Config, .. }
        ));
    }

    #[test]
    fn evictions_and_deadlines_are_told_apart() {
        let tasks = [task(0, TaskKind::Fetch, "fetch")];
        let deadline = pod(json!({ "phase": "Failed", "reason": "DeadlineExceeded" }));
        assert!(matches!(
            observe(&deadline, &tasks, 0, 120).outcome,
            Outcome::Failed { failure: Failure::Timeout, .. }
        ));
        let disk = pod(
            json!({ "phase": "Failed", "reason": "Evicted", "message": "Pod ephemeral local storage usage exceeds the total limit" }),
        );
        assert!(matches!(observe(&disk, &tasks, 0, 120).outcome, Outcome::Failed { failure: Failure::Storage, .. }));
        let pressure =
            pod(json!({ "phase": "Failed", "reason": "Evicted", "message": "The node was low on resource: memory." }));
        assert!(matches!(observe(&pressure, &tasks, 0, 120).outcome, Outcome::Infra { .. }));
    }

    #[test]
    fn a_service_that_restarts_fails_the_build_but_buildkit_restarts_are_left_to_retry() {
        let tasks = [task(0, TaskKind::Fetch, "fetch"), task(1, TaskKind::Verify, "verify-smoke")];
        let mut service = waiting("verify-smoke-svc0", "app", "CrashLoopBackOff");
        service["restartCount"] = json!(1);
        let crashing = pod(json!({ "phase": "Pending", "initContainerStatuses": [
            terminated("fetch", 0, "Completed"), service] }));
        assert!(matches!(observe(&crashing, &tasks, 0, 120).outcome, Outcome::Failed { failure: Failure::Step, .. }));
        let mut buildkit = waiting("buildkitd", "buildkit", "CrashLoopBackOff");
        buildkit["restartCount"] = json!(1);
        let restarted = pod(json!({ "phase": "Pending", "initContainerStatuses": [
            terminated("fetch", 0, "Completed"), buildkit] }));
        let seen = observe(&restarted, &tasks, 0, 120);
        assert!(matches!(seen.outcome, Outcome::Pending { .. }));
        assert_eq!(seen.sidecar_restarts, vec![("buildkitd".to_owned(), 1)]);
    }
}
