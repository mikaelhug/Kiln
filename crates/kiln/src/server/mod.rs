pub mod checks;
mod demo;
pub mod engine;
pub mod fake;
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
    if !Settings::github_configured() {
        let listen = Settings::listen()?;
        let app = web::unconnected(Settings::public_url()).layer(tower_http::trace::TraceLayer::new_for_http());
        let listener = tokio::net::TcpListener::bind(listen).await.with_context(|| format!("listening on {listen}"))?;
        tracing::warn!(%listen, "no GitHub App is configured; serving the connect page until KILN_APP_ID is set");
        axum::serve(listener, app).with_graceful_shutdown(shutdown()).await?;
        return Ok(());
    }
    let settings = Arc::new(Settings::from_env()?);
    std::fs::create_dir_all(&settings.data_dir).with_context(|| format!("creating {}", settings.data_dir.display()))?;
    let store = Arc::new(Store::open(&settings.data_dir.join("kiln.db"))?);
    let (events, _) = broadcast::channel(4096);
    let logs = Arc::new(Logs::new(settings.data_dir.clone(), events));
    let platform = Arc::new(live::Live::connect(&settings).await?);
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
    run(engine, Some(oauth), false, settings.listen).await
}

async fn run<P: Platform>(
    engine: Arc<Engine<P>>,
    oauth: Option<OAuthApp>,
    demo: bool,
    listen: SocketAddr,
) -> Result<()> {
    let app = web::router(Arc::new(web::Web { engine, oauth, demo, deliveries: Default::default() }))
        .layer(tower_http::trace::TraceLayer::new_for_http());
    let listener = tokio::net::TcpListener::bind(listen).await.with_context(|| format!("listening on {listen}"))?;
    tracing::info!(%listen, "kiln is listening");
    axum::serve(listener, app).with_graceful_shutdown(shutdown()).await?;
    Ok(())
}

async fn shutdown() {
    let mut terminate =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("signal handler");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = terminate.recv() => {}
    }
    tracing::info!("shutting down");
}
