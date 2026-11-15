use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use kiln_github::unix_now;
use kiln_runner::{Marker, render_marker};
use kiln_store::{
    BuildState, BuildUpdate, Failure, NewBuild, Phase, RepoSync, StepUpdate, Store, TaskKind, TaskPlan, TaskState,
    TaskUpdate, Trigger,
};
use tokio::sync::broadcast;

use super::engine::Engine;
use super::fake::{Fake, Pace};
use super::logs::Logs;
use super::settings::Settings;

const WEBSHOP: &str = r#"version = 1

[[images]]
name = "webshop"
args = { DEPLOYMENT_ID = "${sha}" }

[test.unit]
image = "node:26"
cache = ["/root/.npm"]
steps = ["npm ci", "npx prisma generate", "npm run typecheck", "npm run lint", "npm test"]

[verify.smoke]
image = "curlimages/curl:8"
steps = ["curl --fail --retry 10 --retry-connrefused http://127.0.0.1:3000/api/health"]
services = [{ name = "app", candidate = "webshop" }]
"#;

const API: &str = r#"version = 1

[[images]]
name = "api-migrate"
file = "Dockerfile.migrate"

[[images]]
name = "api"

[test.unit]
image = "node:26"
steps = ["npm ci", "npm test"]
"#;

const LOGGER: &str = r#"version = 1

[release]
mode = "tags"

[[images]]
name = "logger"

[test.cargo]
image = "rust:1.99"
steps = ["cargo test --locked"]
"#;

const MESSAGES: [&str; 8] = [
    "Speed up the product search",
    "Fix the currency rounding in the cart",
    "Add order history to the account page",
    "Upgrade Prisma",
    "Show stock levels on product cards",
    "Retry failed webhook deliveries",
    "Tighten the session cookie settings",
    "Translate the checkout into Swedish",
];

fn sha(seed: u64) -> String {
    let mut value = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..5)
        .map(|_| {
            value ^= value << 13;
            value ^= value >> 7;
            value ^= value << 17;
            format!("{:08x}", value as u32)
        })
        .collect()
}

pub async fn demo(bind: std::net::IpAddr, port: u16) -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let dir = std::env::temp_dir().join(format!("kiln-demo-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let public_url = format!("http://localhost:{port}");
    let mut settings = Settings::local(dir.clone(), &public_url);
    settings.listen = (bind, port).into();
    settings.poll_secs = 15;
    let settings = Arc::new(settings);
    let fake = Arc::new(Fake::new(Pace::demo()));
    fake.add_repo(1, "kiln-demo", "webshop");
    fake.add_repo(2, "kiln-demo", "api");
    fake.add_repo(3, "kiln-demo", "logger");
    fake.add_repo(4, "kiln-demo", "docs-site");
    fake.push(1, &sha(1), MESSAGES[0], Some(WEBSHOP));
    fake.push(2, &sha(2), "Add the orders endpoint", Some(API));
    fake.push(3, &sha(3), "Parse gzip uploads", Some(LOGGER));
    fake.push(4, &sha(4), "Write the getting started guide", None);
    fake.tag(2, "v0.4.1", &sha(99));
    fake.tag(3, "v0.8.0", &sha(98));
    let store = Arc::new(Store::open(&dir.join("kiln.db"))?);
    let (events, _) = broadcast::channel(4096);
    let logs = Arc::new(Logs::new(dir.clone(), events));
    seed(&store, &logs, &fake)?;
    let engine = Engine::new(store, Arc::new(fake.clone()), settings.clone(), logs);
    engine.start();
    engine.poke(None);
    tokio::spawn(activity(fake.clone(), engine.clone()));
    println!(
        "\n  Kiln demo running at {public_url}\n  Everything is simulated: no GitHub, registry or cluster is contacted.\n  Data lives in {}\n",
        dir.display()
    );
    super::run(engine, None, None, true, settings.listen).await?;
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

async fn activity(fake: Arc<Fake>, engine: Arc<Engine<Arc<Fake>>>) {
    tokio::time::sleep(Duration::from_secs(20)).await;
    fake.fail_container("test-unit");
    fake.open_pull(1, 12, &sha(500), "Try a new checkout layout", WEBSHOP);
    engine.poke(Some(1));
    tokio::time::sleep(Duration::from_secs(25)).await;
    fake.tag(3, "v0.9.0", &sha(3));
    engine.poke(Some(3));
    let mut round = 1u64;
    loop {
        tokio::time::sleep(Duration::from_secs(70)).await;
        round += 1;
        let repo = if round.is_multiple_of(3) { 2 } else { 1 };
        let message = MESSAGES[(round as usize) % MESSAGES.len()];
        let config = if repo == 1 { WEBSHOP } else { API };
        fake.push(repo, &sha(round * 7 + repo as u64), message, Some(config));
        engine.poke(Some(repo));
    }
}

struct History {
    repo: i64,
    config: &'static str,
    images: &'static [&'static str],
    tests: &'static [(&'static str, &'static [&'static str])],
    verify: bool,
    base: &'static str,
    builds: u64,
    spacing: i64,
    releases: bool,
}

const HISTORY: [History; 3] = [
    History {
        repo: 1,
        config: WEBSHOP,
        images: &["webshop"],
        tests: &[("unit", &["npm ci", "npx prisma generate", "npm run typecheck", "npm run lint", "npm test"])],
        verify: true,
        base: "0.0.1",
        builds: 55,
        spacing: 3 * 3600,
        releases: true,
    },
    History {
        repo: 2,
        config: API,
        images: &["api-migrate", "api"],
        tests: &[("unit", &["npm ci", "npm test"])],
        verify: false,
        base: "0.4.2",
        builds: 18,
        spacing: 7 * 3600,
        releases: true,
    },
    History {
        repo: 3,
        config: LOGGER,
        images: &["logger"],
        tests: &[("cargo", &["cargo test --locked"])],
        verify: false,
        base: "0.8.0",
        builds: 9,
        spacing: 11 * 3600,
        releases: false,
    },
];

fn seed(store: &Store, logs: &Logs, fake: &Fake) -> Result<()> {
    let now = unix_now();
    for (id, name) in [(1, "webshop"), (2, "api"), (3, "logger"), (4, "docs-site")] {
        store.upsert_repo(&RepoSync {
            id,
            installation_id: 1,
            owner: "kiln-demo".into(),
            name: name.into(),
            default_branch: "main".into(),
            private: true,
            archived: false,
            seen_at: now,
        })?;
    }
    let mut timeline = Vec::new();
    for history in &HISTORY {
        let shown = history.builds.min(24);
        for index in 0..shown {
            let rc = history.builds - shown + index + 1;
            let seed = history.repo as u64 * 1000 + rc;
            let created = now - (shown - index) as i64 * history.spacing - (seed % 1800) as i64 - 600;
            timeline.push((created, history, index, rc, seed));
        }
    }
    timeline.sort_by_key(|entry| entry.0);
    for (created, history, index, rc, seed) in timeline {
        let outcome = match seed % 9 {
            4 => BuildState::Failed,
            7 if index % 2 == 0 => BuildState::Cancelled,
            _ => BuildState::Succeeded,
        };
        let version = format!("{}-rc.{rc}", history.base);
        let detail = match outcome {
            BuildState::Failed => "unit failed: Error: 1 test failed: renders the checkout page".to_owned(),
            BuildState::Cancelled => "Cancelled by demo".to_owned(),
            _ if history.releases => format!("Released {version}"),
            _ => "Candidate ready; releases on tags".to_owned(),
        };
        let id = store
            .insert_build(&NewBuild {
                repo_id: history.repo,
                trigger: Trigger::Push,
                ref_name: "main".into(),
                pr_number: None,
                sha: sha(seed),
                message: Some(MESSAGES[(seed as usize) % MESSAGES.len()].into()),
                dedupe_key: format!("seed:{}:{rc}", history.repo),
                actor: (outcome == BuildState::Cancelled).then(|| "demo".into()),
                trusted: true,
                config: Some(history.config.into()),
                state: BuildState::Queued,
                failure: None,
                detail: None,
                rerun_of: None,
                created_at: created,
            })?
            .context("seeding a build")?;
        let started = created + 4 + (seed % 20) as i64;
        let finished = seed_tasks(store, logs, history, id, started, seed, outcome)?;
        store.update_build(
            id,
            &BuildUpdate {
                state: Some(outcome),
                phase: Some(Some(Phase::Release)),
                failure: Some((outcome == BuildState::Failed).then_some(Failure::Step)),
                detail: Some(Some(detail)),
                version: Some(history.releases.then(|| version.clone())),
                attempt: Some(1),
                started_at: Some(started),
                finished_at: Some(finished),
                ..Default::default()
            },
        )?;
        if outcome == BuildState::Succeeded {
            for image in history.images {
                let digest = format!("sha256:{:064x}", seed.wrapping_mul(2654435761) ^ image.len() as u64);
                fake.publish(&format!("kiln-demo/{image}"), &format!("sha-{}", sha(seed)), &digest);
                if history.releases {
                    fake.publish(&format!("kiln-demo/{image}"), &version, &digest);
                    store.set_released(id, image, &version)?;
                    store.record_release(history.repo, image, &version, finished)?;
                }
            }
        }
    }
    fake.publish("kiln-demo/logger", "0.8.0", "sha256:old");
    Ok(())
}

fn seed_tasks(
    store: &Store,
    logs: &Logs,
    history: &History,
    id: i64,
    started: i64,
    seed: u64,
    outcome: BuildState,
) -> Result<i64> {
    let mut plans =
        vec![TaskPlan { kind: TaskKind::Fetch, name: "fetch".into(), container: "fetch".into(), steps: vec![] }];
    for (name, steps) in history.tests {
        plans.push(TaskPlan {
            kind: TaskKind::Test,
            name: (*name).into(),
            container: format!("test-{name}"),
            steps: steps.iter().map(|s| (*s).to_owned()).collect(),
        });
    }
    for image in history.images {
        plans.push(TaskPlan {
            kind: TaskKind::Image,
            name: (*image).into(),
            container: format!("image-{image}"),
            steps: vec![],
        });
    }
    plans.push(TaskPlan { kind: TaskKind::Push, name: "push".into(), container: "push".into(), steps: vec![] });
    if history.verify {
        plans.push(TaskPlan {
            kind: TaskKind::Verify,
            name: "smoke".into(),
            container: "verify-smoke".into(),
            steps: vec!["curl --fail --retry 10 --retry-connrefused http://127.0.0.1:3000/api/health".into()],
        });
    }
    store.replace_tasks(id, 1, &plans)?;
    store.set_images(id, &history.images.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>())?;
    let mut clock = started;
    let mut stopped = false;
    for (ordinal, plan) in plans.iter().enumerate() {
        let ordinal = ordinal as i64;
        if stopped {
            store.update_task(
                id,
                1,
                ordinal,
                &TaskUpdate { state: Some(TaskState::Skipped), log_final: Some(true), ..Default::default() },
            )?;
            continue;
        }
        let begin = clock;
        let failing = outcome == BuildState::Failed && plan.kind == TaskKind::Test;
        let cancelled = outcome == BuildState::Cancelled && plan.kind == TaskKind::Image;
        let mut text = String::new();
        for (index, step) in plan.steps.iter().enumerate() {
            let length = 3 + ((seed + index as u64 * 7) % 40) as i64;
            text.push_str(&render_marker(&Marker::StepStart { index, at: clock }));
            text.push_str(&format!("\n$ {step}\n"));
            for line in demo_output(step) {
                text.push_str(line);
                text.push('\n');
            }
            let last = index + 1 == plan.steps.len();
            let exit = if failing && last { 1 } else { 0 };
            if exit != 0 {
                text.push_str("Error: 1 test failed: renders the checkout page (expected 200, got 500)\n");
            }
            text.push_str(&render_marker(&Marker::StepEnd { index, exit, at: clock + length }));
            text.push('\n');
            store.update_step(
                id,
                1,
                ordinal,
                index as i64,
                &StepUpdate {
                    state: if exit == 0 { TaskState::Passed } else { TaskState::Failed },
                    exit_code: Some(i64::from(exit)),
                    started_at: Some(clock),
                    finished_at: Some(clock + length),
                    log_offset: None,
                },
            )?;
            clock += length;
        }
        match plan.kind {
            TaskKind::Fetch => {
                text.push_str("Downloading the source at the commit\nUnpacked 1,284 files (6.1 MB)\n");
                clock += 2;
            }
            TaskKind::Image => {
                text.push_str("#1 [internal] load build definition from Dockerfile\n#4 [deps 2/3] RUN npm ci\n#4 CACHED\n#6 exporting to oci image format\n#6 DONE 2.4s\n");
                clock += 30 + (seed % 70) as i64;
            }
            TaskKind::Push => {
                for image in history.images {
                    text.push_str(&format!("Pushed {image} in 3.2s\n"));
                }
                clock += 4;
            }
            _ => {}
        }
        let state = if failing {
            TaskState::Failed
        } else if cancelled {
            TaskState::Cancelled
        } else {
            TaskState::Passed
        };
        logs.replace(id, 1, ordinal, &text)?;
        store.update_task(
            id,
            1,
            ordinal,
            &TaskUpdate {
                state: Some(state),
                exit_code: Some(if state == TaskState::Failed { 1 } else { 0 }),
                started_at: Some(begin),
                finished_at: Some(clock),
                log_final: Some(true),
                log_bytes: Some(text.len() as i64),
                ..Default::default()
            },
        )?;
        if state == TaskState::Passed && plan.kind == TaskKind::Image {
            store.set_candidate(
                id,
                &plan.name,
                &format!("sha256:{:064x}", seed.wrapping_mul(2654435761) ^ plan.name.len() as u64),
            )?;
        }
        stopped = state != TaskState::Passed;
    }
    Ok(clock + 3)
}

fn demo_output(step: &str) -> &'static [&'static str] {
    if step.contains("npm ci") {
        &["added 812 packages, and audited 813 packages in 14s", "found 0 vulnerabilities"]
    } else if step.contains("lint") {
        &["> eslint .", "\u{1b}[32m✔\u{1b}[0m No problems found"]
    } else if step.contains("typecheck") {
        &["> tsc --noEmit"]
    } else if step.contains("cargo") {
        &[
            "   Compiling logger v0.8.0",
            "    Finished test profile in 41.2s",
            "test result: \u{1b}[32mok\u{1b}[0m. 64 passed; 0 failed",
        ]
    } else if step.contains("test") {
        &[
            " \u{1b}[32m✓\u{1b}[0m src/lib/cart.test.ts (14 tests) 41ms",
            " \u{1b}[32m✓\u{1b}[0m src/lib/auth.test.ts (9 tests) 28ms",
            " Test Files  2 passed (2)",
            "      Tests  23 passed (23)",
        ]
    } else if step.contains("curl") {
        &["{\"status\":\"ok\"}"]
    } else {
        &[]
    }
}
