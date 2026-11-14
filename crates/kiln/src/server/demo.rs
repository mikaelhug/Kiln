use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use kiln_store::Store;
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

pub async fn demo(port: u16) -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let dir = std::env::temp_dir().join(format!("kiln-demo-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let public_url = format!("http://localhost:{port}");
    let mut settings = Settings::local(dir.clone(), &public_url);
    settings.listen = ([127, 0, 0, 1], port).into();
    settings.poll_secs = 15;
    let settings = Arc::new(settings);
    let fake = Arc::new(Fake::new(Pace::demo()));
    fake.add_repo(1, "kiln-demo", "webshop");
    fake.add_repo(2, "kiln-demo", "api");
    fake.add_repo(3, "kiln-demo", "logger");
    fake.add_repo(4, "kiln-demo", "docs-site");
    fake.publish("kiln-demo/webshop", "0.0.1-rc.55", "sha256:old");
    fake.publish("kiln-demo/api", "0.4.2-rc.18", "sha256:old");
    fake.publish("kiln-demo/logger", "0.8.0", "sha256:old");
    fake.push(1, &sha(1), MESSAGES[0], Some(WEBSHOP));
    fake.push(2, &sha(2), "Add the orders endpoint", Some(API));
    fake.push(3, &sha(3), "Parse gzip uploads", Some(LOGGER));
    fake.push(4, &sha(4), "Write the getting started guide", None);
    fake.tag(2, "v0.4.1", &sha(99));
    fake.tag(3, "v0.8.0", &sha(98));
    let store = Arc::new(Store::open(&dir.join("kiln.db"))?);
    let (events, _) = broadcast::channel(4096);
    let logs = Arc::new(Logs::new(dir.clone(), events));
    let engine = Engine::new(store, Arc::new(fake.clone()), settings.clone(), logs);
    engine.start();
    engine.poke(None);
    tokio::spawn(activity(fake.clone(), engine.clone()));
    println!(
        "\n  Kiln demo running at {public_url}\n  Everything is simulated: no GitHub, registry or cluster is contacted.\n  Data lives in {}\n",
        dir.display()
    );
    super::run(engine, None, true, settings.listen).await?;
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
