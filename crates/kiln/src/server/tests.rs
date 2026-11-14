use std::sync::Arc;
use std::time::Duration;

use kiln_github::CheckConclusion;
use kiln_store::{Build, BuildFilter, BuildState, Failure, Store, Trigger};
use tokio::sync::broadcast;

use super::engine::Engine;
use super::fake::{Fake, Pace};
use super::logs::Logs;
use super::settings::Settings;

const CONFIG: &str = r#"
version = 1

[[images]]
name = "web"
args = { DEPLOYMENT_ID = "${sha}" }

[test.unit]
image = "node:26"
steps = ["npm ci", "npm test"]

[verify.smoke]
image = "curlimages/curl:8"
steps = ["curl -f http://127.0.0.1:8080/healthz"]
services = [{ name = "app", candidate = "web" }]
"#;

struct Harness {
    engine: Arc<Engine<Arc<Fake>>>,
    fake: Arc<Fake>,
    _dir: tempfile::TempDir,
}

fn sha(n: u8) -> String {
    format!("{n:02x}").repeat(20)
}

async fn harness() -> Harness {
    harness_with(|_| {}).await
}

async fn harness_with(adjust: impl FnOnce(&mut Settings)) -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let fake = Arc::new(Fake::new(Pace::instant()));
    fake.add_repo(1, "kiln-demo", "web");
    fake.publish("kiln-demo/web", "0.0.1-rc.55", "sha256:old");
    let mut settings =
        Settings { retry_base_secs: 0, ..Settings::local(dir.path().to_owned(), "https://kiln.example") };
    adjust(&mut settings);
    let settings = Arc::new(settings);
    let (events, _) = broadcast::channel(1024);
    let logs = Arc::new(Logs::new(dir.path().to_owned(), events));
    let engine = Engine::new(Arc::new(Store::in_memory().unwrap()), Arc::new(fake.clone()), settings, logs);
    engine.start();
    Harness { engine, fake, _dir: dir }
}

impl Harness {
    async fn sync(&self) {
        self.engine.reconcile_all(true).await.unwrap();
        self.engine.poke(Some(1));
    }

    fn builds(&self) -> Vec<Build> {
        let mut builds = self.engine.store.builds(&BuildFilter { limit: 100, ..Default::default() }).unwrap();
        builds.reverse();
        builds
    }

    async fn until(&self, what: &str, done: impl Fn(&[Build]) -> bool) -> Vec<Build> {
        for _ in 0..400 {
            let builds = self.builds();
            if done(&builds) {
                return builds;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!(
            "timed out waiting for {what}: {:#?}",
            self.builds().iter().map(|b| (b.number, b.state, b.detail.clone())).collect::<Vec<_>>()
        );
    }
}

#[tokio::test]
async fn a_push_is_tested_built_verified_and_released_above_existing_versions() {
    let h = harness().await;
    h.fake.push(1, &sha(1), "Add checkout", Some(CONFIG));
    h.sync().await;
    let builds = h.until("the release", |b| b.first().is_some_and(|b| b.state.is_finished())).await;
    let build = &builds[0];
    assert_eq!(build.state, BuildState::Succeeded, "{:?}", build.detail);
    assert_eq!(build.version.as_deref(), Some("0.0.1-rc.56"));
    assert!(h.fake.registry_tag("kiln-demo/web", "0.0.1-rc.56").is_some());
    let tasks = h.engine.store.tasks(build.id, 1).unwrap();
    assert!(tasks.iter().all(|t| t.log_final), "every log is final");
    let unit = tasks.iter().find(|t| t.name == "unit").unwrap();
    assert_eq!(unit.steps.iter().map(|s| s.state.as_str()).collect::<Vec<_>>(), ["passed", "passed"]);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(h.fake.pod_names().is_empty(), "finished pods are deleted");
    assert!(h.fake.revoked() >= 1, "the fetch token is revoked");
    let check = h.fake.checks().pop().unwrap();
    assert_eq!(check.conclusion, Some(CheckConclusion::Success));
    assert_eq!(check.output.unwrap().title, "Released 0.0.1-rc.56");
}

#[tokio::test]
async fn a_newer_push_supersedes_the_older_one() {
    let h = harness().await;
    h.fake.push(1, &sha(1), "First", Some(CONFIG));
    h.sync().await;
    h.fake.push(1, &sha(2), "Second", Some(CONFIG));
    h.sync().await;
    let builds = h.until("both builds to finish", |b| b.len() == 2 && b.iter().all(|b| b.state.is_finished())).await;
    assert_eq!(builds[0].state, BuildState::Superseded);
    assert_eq!(builds[0].detail.as_deref(), Some("Superseded by #2"));
    assert_eq!(builds[1].state, BuildState::Succeeded);
}

#[tokio::test]
async fn pull_requests_only_run_tests_and_never_release() {
    let h = harness().await;
    h.fake.push(1, &sha(1), "Base", Some(CONFIG));
    h.sync().await;
    h.until("the base build", |b| b.first().is_some_and(|b| b.state.is_finished())).await;
    h.fake.open_pull(1, 7, &sha(3), "Try a thing", CONFIG);
    h.sync().await;
    let builds = h.until("the PR build", |b| b.len() == 2 && b[1].state.is_finished()).await;
    let pr = &builds[1];
    assert_eq!(pr.trigger, Trigger::PullRequest);
    assert_eq!(pr.state, BuildState::Succeeded);
    assert!(!pr.trusted);
    assert_eq!(pr.version, None);
    let kinds: Vec<_> = h.engine.store.tasks(pr.id, 1).unwrap().iter().map(|t| t.kind.as_str()).collect();
    assert_eq!(kinds, ["fetch", "test"]);
    assert!(h.engine.store.images(pr.id).unwrap().is_empty());
}

#[tokio::test]
async fn a_tag_on_a_verified_commit_releases_the_same_image_without_rebuilding() {
    let h = harness().await;
    h.fake.push(1, &sha(1), "Ready", Some(CONFIG));
    h.sync().await;
    h.until("the main build", |b| b.first().is_some_and(|b| b.state == BuildState::Succeeded)).await;
    h.fake.tag(1, "v1.0.0", &sha(1));
    h.sync().await;
    let builds = h.until("the tag build", |b| b.len() == 2 && b[1].state.is_finished()).await;
    let tag = &builds[1];
    assert_eq!(tag.state, BuildState::Succeeded, "{:?}", tag.detail);
    assert_eq!(tag.version.as_deref(), Some("1.0.0"));
    assert!(h.engine.store.tasks(tag.id, 1).unwrap().is_empty(), "no pod ran");
    assert_eq!(h.fake.registry_tag("kiln-demo/web", "1.0.0"), h.fake.registry_tag("kiln-demo/web", "0.0.1-rc.56"));
}

#[tokio::test]
async fn a_failing_test_stops_the_release() {
    let h = harness().await;
    h.fake.fail_container("test-unit");
    h.fake.push(1, &sha(1), "Break it", Some(CONFIG));
    h.sync().await;
    let builds = h.until("the failure", |b| b.first().is_some_and(|b| b.state.is_finished())).await;
    assert_eq!(builds[0].state, BuildState::Failed);
    assert_eq!(builds[0].failure, Some(Failure::Step));
    assert!(h.fake.registry_tag("kiln-demo/web", "0.0.1-rc.56").is_none());
    let unit = h.engine.store.tasks(builds[0].id, 1).unwrap().into_iter().find(|t| t.name == "unit").unwrap();
    assert_eq!(unit.steps.last().unwrap().exit_code, Some(1));
    let tasks = h.engine.store.tasks(builds[0].id, 1).unwrap();
    assert!(tasks.iter().filter(|t| t.ordinal > unit.ordinal).all(|t| t.state.as_str() == "skipped"));
}

#[tokio::test]
async fn a_lost_build_pod_is_retried() {
    let h = harness().await;
    h.fake.lose_next_pod("test-unit");
    h.fake.push(1, &sha(1), "Survive a drain", Some(CONFIG));
    h.sync().await;
    let builds = h.until("the retried build", |b| b.first().is_some_and(|b| b.state.is_finished())).await;
    assert_eq!(builds[0].state, BuildState::Succeeded, "{:?}", builds[0].detail);
    assert_eq!(builds[0].attempt, 2);
}

#[tokio::test]
async fn an_invalid_config_fails_with_annotations_and_skip_ci_is_honoured() {
    let h = harness().await;
    h.fake.push(1, &sha(1), "Typo", Some("version = 1\n\n[release]\nmodee = \"rc\"\n"));
    h.sync().await;
    let builds = h.until("the config failure", |b| b.first().is_some_and(|b| b.state.is_finished())).await;
    assert_eq!(builds[0].failure, Some(Failure::Config));
    for _ in 0..100 {
        if !h.fake.checks().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let check = h.fake.checks().pop().unwrap();
    let annotations = check.output.unwrap().annotations;
    assert_eq!(annotations.first().map(|a| a.start_line), Some(4));
    h.fake.push(1, &sha(2), "Docs only [skip ci]", Some(CONFIG));
    h.sync().await;
    let builds = h.until("the skipped build", |b| b.len() == 2).await;
    assert_eq!(builds[1].state, BuildState::Skipped);
}

#[tokio::test]
async fn versions_published_outside_kiln_are_flagged_but_existing_ones_are_not() {
    let h = harness().await;
    h.fake.push(1, &sha(1), "Ship", Some(CONFIG));
    h.sync().await;
    h.until("the release", |b| b.first().is_some_and(|b| b.state == BuildState::Succeeded)).await;
    h.engine.scan_registry().await.unwrap();
    h.fake.push(1, &sha(2), "Ship again", Some(CONFIG));
    h.sync().await;
    h.until("the second release", |b| b.len() == 2 && b[1].state == BuildState::Succeeded).await;
    h.engine.scan_registry().await.unwrap();
    assert_eq!(h.engine.store.repo(1).unwrap().unwrap().drift, None, "the baseline and Kiln's own releases are known");
    h.fake.publish("kiln-demo/web", "0.0.2", "sha256:elsewhere");
    h.engine.scan_registry().await.unwrap();
    assert_eq!(h.engine.store.repo(1).unwrap().unwrap().drift.as_deref(), Some("web:0.0.2"));
}

#[tokio::test]
async fn a_tag_on_a_commit_outside_the_default_branch_is_never_built_or_released() {
    let h = harness().await;
    h.fake.push(1, &sha(1), "Base", Some(CONFIG));
    h.sync().await;
    h.until("the base build", |b| b.first().is_some_and(|b| b.state.is_finished())).await;
    h.fake.push(1, &sha(1), "Base", Some(CONFIG));
    h.fake.push(1, &sha(9), "Sneaky", Some(CONFIG));
    h.fake.push(1, &sha(1), "Base", Some(CONFIG));
    h.fake.tag_off_branch(1, "v9.9.9", &sha(9));
    h.sync().await;
    let builds =
        h.until("the tag build", |b| b.iter().any(|b| b.trigger == Trigger::Tag && b.state.is_finished())).await;
    let tag = builds.iter().find(|b| b.trigger == Trigger::Tag).unwrap();
    assert!(!tag.trusted);
    assert_eq!(tag.state, BuildState::Failed, "{:?}", tag.detail);
    assert!(h.engine.store.tasks(tag.id, 1).unwrap().is_empty(), "no pod ran");
    assert!(h.fake.registry_tag("kiln-demo/web", "9.9.9").is_none());
}

#[tokio::test]
async fn a_repository_cannot_release_into_another_repositorys_image() {
    let h = harness().await;
    h.fake.push(1, &sha(1), "Ship", Some(CONFIG));
    h.sync().await;
    h.until("the release", |b| b.first().is_some_and(|b| b.state == BuildState::Succeeded)).await;
    h.fake.add_repo(2, "kiln-demo", "intruder");
    h.fake.push(2, &sha(2), "Pretend to be web", Some(CONFIG));
    h.engine.reconcile_all(true).await.unwrap();
    let builds = h.until("the intruder build", |b| b.iter().any(|b| b.repo_id == 2 && b.state.is_finished())).await;
    let intruder = builds.iter().find(|b| b.repo_id == 2).unwrap();
    assert_eq!(intruder.state, BuildState::Failed);
    assert!(
        intruder.detail.as_deref().unwrap_or_default().contains("belongs to kiln-demo/web"),
        "{:?}",
        intruder.detail
    );
}

#[tokio::test]
async fn a_release_that_keeps_failing_gives_up_and_frees_its_slot() {
    let h = harness_with(|s| {
        s.release_window_secs = 0;
        s.slots = 1;
    })
    .await;
    h.fake.refuse_promotes();
    h.fake.push(1, &sha(1), "Ship", Some(CONFIG));
    h.sync().await;
    h.until("the release to reach the registry", |b| {
        b.first().is_some_and(|b| b.state == BuildState::Releasing || b.state.is_finished())
    })
    .await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    h.engine.release(h.builds()[0].id).await;
    let builds = h.until("the release to give up", |b| b.first().is_some_and(|b| b.state.is_finished())).await;
    assert_eq!(builds[0].state, BuildState::Failed);
    assert_eq!(builds[0].failure, Some(Failure::Release));
}

#[tokio::test]
async fn a_releasing_build_frees_its_slot_and_can_be_cancelled_before_anything_is_published() {
    let h = harness_with(|s| s.slots = 1).await;
    h.fake.refuse_promotes();
    h.fake.push(1, &sha(1), "Ship", Some(CONFIG));
    h.sync().await;
    h.until("the release", |b| b.first().is_some_and(|b| b.state == BuildState::Releasing)).await;
    h.fake.add_repo(2, "kiln-demo", "other");
    h.fake.push(2, &sha(2), "Other", Some(&CONFIG.replace("\"web\"", "\"other\"")));
    h.engine.reconcile_all(true).await.unwrap();
    h.engine.poke(Some(2));
    h.until("the other repository to start", |b| b.iter().any(|b| b.repo_id == 2 && b.state != BuildState::Queued))
        .await;
    let releasing = h.builds()[0].id;
    h.engine.cancel(releasing, "tester").unwrap();
    let builds = h.until("the cancel", |b| b[0].state.is_finished()).await;
    assert_eq!(builds[0].state, BuildState::Cancelled);
    assert!(h.fake.registry_tag("kiln-demo/web", "0.0.1-rc.56").is_none());
}
