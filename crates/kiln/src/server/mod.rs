pub mod checks;
mod demo;
pub mod engine;
pub mod fake;
pub mod inspect;
pub mod live;
pub mod logs;
pub mod platform;
pub mod settings;
#[cfg(test)]
mod tests;
mod web;

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use kiln_github::OAuthApp;
use kiln_store::Store;
use tokio::sync::broadcast;

use self::engine::Engine;
use self::logs::Logs;
use self::platform::Platform;
use self::settings::Settings;

pub use demo::demo;

pub async fn serve() -> Result<()> {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info,kube=warn".into()),
        )
        .init();
    let listen = Settings::listen()?;
    let data_dir = Settings::data_dir();
    std::fs::create_dir_all(&data_dir).with_context(|| format!("creating {}", data_dir.display()))?;
    let store = Arc::new(Store::open(&data_dir.join("kiln.db"))?);
    if store.github()?.is_none() {
        let connect = Arc::new(web::connect::Connect::new(
            store.clone(),
            Settings::public_url()?,
            Settings::github_web(),
            Settings::github_api(),
        ));
        tracing::warn!(
            code = connect.code,
            "Kiln is not connected to GitHub. Open it in a browser and enter this setup code"
        );
        let app = web::connect::router(connect.clone()).layer(tower_http::trace::TraceLayer::new_for_http());
        let listener = tokio::net::TcpListener::bind(listen).await.with_context(|| format!("listening on {listen}"))?;
        let stop = connect.clone();
        let connected = async move {
            tokio::select! {
                _ = shutdown() => {}
                _ = stop.connected.notified() => {}
            }
        };
        axum::serve(listener, app).with_graceful_shutdown(connected).await?;
    }
    let Some(github) = store.github()? else { return Ok(()) };
    let settings = Arc::new(Settings::from_env(&github)?);
    let (events, _) = broadcast::channel(4096);
    let logs = Arc::new(Logs::new(settings.data_dir.clone(), events));
    let platform = Arc::new(live::Live::connect(&settings).await?);
    let nobody = store.people()?.is_empty();
    let engine = Engine::new(store, platform, settings.clone(), logs);
    engine.start();
    engine.poke(None);
    let oauth = OAuthApp {
        client_id: settings.oauth_client_id.clone(),
        client_secret: settings.oauth_client_secret.clone(),
        redirect_uri: format!("{}/auth/callback", settings.public_url),
        web: settings.github_web.clone(),
        api: settings.github_api.clone(),
    };
    let claim_code = nobody.then(web::connect::setup_code);
    if let Some(code) = &claim_code {
        tracing::warn!(
            code,
            "nobody can sign in yet; the first person to sign in with GitHub claims Kiln with this setup code"
        );
    }
    run(engine, Some(oauth), claim_code, false, settings.listen).await
}

async fn run<P: Platform>(
    engine: Arc<Engine<P>>,
    oauth: Option<OAuthApp>,
    claim_code: Option<String>,
    demo: bool,
    listen: SocketAddr,
) -> Result<()> {
    let app = web::router(Arc::new(web::Web {
        engine,
        oauth,
        demo,
        claim_code,
        claims: Default::default(),
        deliveries: Default::default(),
        usual: Default::default(),
    }))
    .layer(tower_http::trace::TraceLayer::new_for_http());
    let listener = tokio::net::TcpListener::bind(listen).await.with_context(|| format!("listening on {listen}"))?;
    tracing::info!(%listen, "kiln is listening");
    axum::serve(listener, app).with_graceful_shutdown(shutdown()).await?;
    Ok(())
}

pub static RESTART: tokio::sync::Notify = tokio::sync::Notify::const_new();

async fn shutdown() {
    let mut terminate =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("signal handler");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = terminate.recv() => {}
        _ = RESTART.notified() => {}
    }
    tracing::info!("shutting down");
}
