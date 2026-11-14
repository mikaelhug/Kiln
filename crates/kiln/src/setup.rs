use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};
use axum::Router;
use axum::extract::{Query, State};
use axum::response::Html;
use axum::routing::get;
use kiln_github::{CreatedApp, convert_manifest, random_token};
use maud::{DOCTYPE, Markup, html};
use serde::Deserialize;
use tokio::sync::oneshot;

pub struct Options {
    pub url: String,
    pub org: Option<String>,
    pub name: Option<String>,
    pub out: PathBuf,
    pub port: u16,
    pub browser: bool,
    pub github: String,
    pub api: String,
}

struct Flow {
    state: String,
    manifest: String,
    action: String,
    api: String,
    out: PathBuf,
    done: Mutex<Option<oneshot::Sender<Result<CreatedApp, String>>>>,
}

#[derive(Deserialize)]
struct Callback {
    code: Option<String>,
    state: Option<String>,
}

pub fn public_url(url: &str) -> Result<String> {
    let url = url.trim_end_matches('/');
    let Some(host) = url.strip_prefix("https://") else {
        bail!("--url must be the https address Kiln will be reachable at, like https://kiln.example.com");
    };
    if host.is_empty() || host.contains('/') || host.contains('?') || host.contains('#') {
        bail!("--url must be an origin without a path, like https://kiln.example.com");
    }
    Ok(url.to_owned())
}

fn manifest(url: &str, name: &str, redirect: &str) -> serde_json::Value {
    serde_json::json!({
        "name": name,
        "url": url,
        "description": "Self-hosted CI: tests, image builds and releases.",
        "hook_attributes": { "url": format!("{url}/hooks/github"), "active": true },
        "redirect_url": redirect,
        "callback_urls": [format!("{url}/auth/callback")],
        "public": false,
        "request_oauth_on_install": false,
        "default_permissions": {
            "contents": "read",
            "checks": "write",
            "pull_requests": "read",
            "metadata": "read"
        },
        "default_events": ["push", "pull_request", "check_run", "create", "repository"]
    })
}

pub async fn run(options: Options) -> Result<ExitCode> {
    let url = public_url(&options.url)?;
    if options.out.exists() {
        bail!("{} already exists; choose another --out or move it away", options.out.display());
    }
    let host = url.trim_start_matches("https://");
    let name = options.name.clone().unwrap_or_else(|| format!("Kiln {host}").chars().take(34).collect());
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", options.port))
        .await
        .with_context(|| format!("listening on 127.0.0.1:{}", options.port))?;
    let local = listener.local_addr()?;
    let redirect = format!("http://127.0.0.1:{}/callback", local.port());
    let state = random_token();
    let github = options.github.trim_end_matches('/');
    let action = match &options.org {
        Some(org) => format!("{github}/organizations/{org}/settings/apps/new?state={state}"),
        None => format!("{github}/settings/apps/new?state={state}"),
    };
    let (sender, receiver) = oneshot::channel();
    let flow = Arc::new(Flow {
        state,
        manifest: manifest(&url, &name, &redirect).to_string(),
        action,
        api: options.api.clone(),
        out: options.out.clone(),
        done: Mutex::new(Some(sender)),
    });
    let app = Router::new().route("/", get(start)).route("/callback", get(callback)).with_state(flow);
    let start_url = format!("http://127.0.0.1:{}/", local.port());
    println!("\nOpen {start_url} in a browser where you are signed in to GitHub.");
    println!("It sends you to GitHub to create the app \"{name}\" for {url}.\n");
    if options.browser {
        open_browser(&start_url);
    }
    let (stop, stopped) = oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = stopped.await;
            })
            .await
    });
    let outcome = tokio::select! {
        outcome = receiver => outcome.map_err(|_| anyhow::anyhow!("the setup page stopped unexpectedly"))?,
        _ = tokio::signal::ctrl_c() => Err("cancelled".to_owned()),
    };
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let _ = stop.send(());
    let _ = server.await;
    let created = outcome.map_err(|error| anyhow::anyhow!(error))?;
    report(&created, &url, &options.out);
    Ok(ExitCode::SUCCESS)
}

fn open_browser(url: &str) {
    let command = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    let _ = std::process::Command::new(command)
        .arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

fn page(title: &str, body: Markup) -> Html<String> {
    Html(
        html! {
            (DOCTYPE)
            html lang="en" {
                head {
                    meta charset="utf-8";
                    meta name="viewport" content="width=device-width, initial-scale=1";
                    title { (title) " · Kiln setup" }
                    style { "body{font:15px/1.55 -apple-system,BlinkMacSystemFont,'Segoe UI',sans-serif;background:#fafaf9;color:#1c1917;display:grid;place-items:center;min-height:100vh;margin:0;padding:24px}main{max-width:460px;background:#fff;border:1px solid #e7e5e4;border-radius:12px;padding:28px 28px 24px;box-shadow:0 12px 32px rgb(28 25 23/.12)}h1{font-size:19px;margin:0 0 8px}p{color:#57534e;margin:0 0 16px}button{font:inherit;font-weight:600;background:#ea580c;color:#fff;border:0;border-radius:8px;padding:10px 16px;cursor:pointer;width:100%}code{font-size:13px;background:#f5f5f4;padding:1px 5px;border-radius:4px}" }
                }
                body { main { (body) } }
            }
        }
        .into_string(),
    )
}

async fn start(State(flow): State<Arc<Flow>>) -> Html<String> {
    page(
        "Create the GitHub App",
        html! {
            h1 { "Create the Kiln GitHub App" }
            p { "GitHub shows the app's name, permissions and events before anything is created. You can rename it there." }
            form method="post" action=(flow.action) {
                input type="hidden" name="manifest" value=(flow.manifest);
                button type="submit" { "Continue on GitHub" }
            }
        },
    )
}

async fn callback(State(flow): State<Arc<Flow>>, Query(query): Query<Callback>) -> Html<String> {
    let (Some(code), Some(state)) = (query.code, query.state) else {
        return page("Not finished", html! { h1 { "Nothing to do here" } p { "Start again from the setup page." } });
    };
    if state != flow.state {
        return page(
            "Not finished",
            html! { h1 { "This link is not from this setup" } p { "Start again from the setup page." } },
        );
    }
    let Some(done) = flow.done.lock().expect("setup lock").take() else {
        return page("Already done", html! { h1 { "The app was already created" } p { "Return to the terminal." } });
    };
    let result = match convert_manifest(&flow.api, &code).await {
        Ok(created) => write_secret(&flow.out, &created).map(|()| created).map_err(|e| format!("{e:#}")),
        Err(error) => Err(format!("GitHub did not hand over the app credentials: {error}")),
    };
    let response = match &result {
        Ok(created) => page(
            "Done",
            html! {
                h1 { "Created " (created.name) }
                p { "The credentials are in " code { (flow.out.display()) } ". Return to the terminal for the next steps." }
            },
        ),
        Err(error) => page("Failed", html! { h1 { "Setup failed" } p { (error) } }),
    };
    let _ = done.send(result);
    response
}

fn write_secret(out: &PathBuf, created: &CreatedApp) -> Result<()> {
    let quote = |value: &str| serde_json::to_string(value).expect("strings serialize");
    let pem: String = created.pem.trim_end().lines().map(|line| format!("    {line}\n")).collect();
    let webhook = created.webhook_secret.clone().context("GitHub did not return a webhook secret")?;
    let username = created.owner.as_ref().filter(|o| o.kind == "User").map(|o| o.login.clone());
    let username = quote(&username.unwrap_or_else(|| "replace with the login that owns the registry token".into()));
    let yaml = format!(
        "apiVersion: v1\nkind: Secret\nmetadata:\n  name: kiln\n  namespace: kiln\ntype: Opaque\nstringData:\n  app-private-key: |\n{pem}  webhook-secret: {}\n  oauth-client-secret: {}\n  registry-password: replace with a token that may push packages\n---\napiVersion: v1\nkind: Secret\nmetadata:\n  name: registry-push\n  namespace: kiln-builds\ntype: Opaque\nstringData:\n  username: {username}\n  password: replace with the same token as registry-password\n",
        quote(&webhook),
        quote(&created.client_secret),
    );
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(out)
        .with_context(|| format!("creating {}", out.display()))?;
    file.write_all(yaml.as_bytes())?;
    Ok(())
}

fn report(created: &CreatedApp, url: &str, out: &std::path::Path) {
    println!("Created the GitHub App \"{}\" ({}).", created.name, created.html_url);
    println!("Wrote its secrets to {}.", out.display());
    println!("Use it as deploy/kubernetes/secrets.yaml, put your registry token in both places it asks for,");
    println!("and encrypt it before committing, for example with sops.\n");
    println!("Set these in deploy/kubernetes/config.yaml:");
    println!("  KILN_PUBLIC_URL: {url}");
    if let Some(owner) = created.owner.as_ref().filter(|o| o.kind == "User") {
        println!("  KILN_REGISTRY_USERNAME: {}", owner.login);
    }
    println!("  KILN_APP_ID: \"{}\"", created.id);
    println!("  KILN_OAUTH_CLIENT_ID: {}", created.client_id);
    if let Some(owner) = &created.owner {
        println!("  KILN_OWNERS: {}", owner.login);
        if owner.kind == "User" {
            println!("  KILN_ALLOWED_USER_IDS: \"{}\"", owner.id);
        } else {
            println!(
                "  KILN_ALLOWED_USER_IDS: the numeric IDs of the people who may sign in (gh api users/<login> -q .id)"
            );
        }
    }
    println!("\nThen install the app on the repositories Kiln should build:");
    println!("  {}/installations/new\n", created.html_url.trim_end_matches('/'));
}
