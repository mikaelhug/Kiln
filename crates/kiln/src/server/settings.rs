use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use kiln_store::GitHubApp;

pub const PUSH_SECRET: &str = "kiln-registry-push";
pub const PULL_SECRET: &str = "kiln-registry-pull";
const BUILDKIT_IMAGE: &str =
    "moby/buildkit:v0.26.0-rootless@sha256:2846ca21f8cadf9c949ab0c04cfe3619e111d6c527f4194d6e5b098abf6aeaff";

#[derive(Clone)]
pub struct Settings {
    pub listen: SocketAddr,
    pub public_url: String,
    pub data_dir: PathBuf,
    pub github_api: String,
    pub github_web: String,
    pub app_id: u64,
    pub app_key: Vec<u8>,
    pub webhook_secrets: Vec<String>,
    pub oauth_client_id: String,
    pub oauth_client_secret: String,
    pub owners: Vec<String>,
    pub include_public: bool,
    pub registry: String,
    pub registry_url: String,
    pub registry_username: String,
    pub registry_password: Option<String>,
    pub slots: usize,
    pub poll_secs: u64,
    pub build_timeout_secs: u64,
    pub max_attempts: i64,
    pub retry_base_secs: i64,
    pub release_window_secs: i64,
    pub repo_secret_prefix: String,
    pub cache_class: Option<String>,
    pub cache_size: String,
    pub runner: kiln_runner::Settings,
}

fn var(name: &str) -> Option<String> {
    std::env::var(name).ok().map(|v| v.trim().to_owned()).filter(|v| !v.is_empty())
}

fn required(name: &str) -> Result<String> {
    var(name).with_context(|| format!("{name} is required"))
}

fn list(name: &str) -> Vec<String> {
    var(name).map(|v| v.split(',').map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()).collect()).unwrap_or_default()
}

fn number<T: std::str::FromStr>(name: &str, default: T) -> Result<T> {
    match var(name) {
        Some(value) => value.parse().map_err(|_| anyhow::anyhow!("{name} must be a number, got {value:?}")),
        None => Ok(default),
    }
}

fn origin(name: &str, value: &str) -> Result<String> {
    let url = url::Url::parse(value)
        .with_context(|| format!("{name} must be a URL like https://kiln.example.com, got {value:?}"))?;
    let local = matches!(url.host_str(), Some("localhost" | "127.0.0.1"));
    if url.scheme() != "https" && !(url.scheme() == "http" && local) {
        bail!("{name} must use https, got {value:?}");
    }
    if !matches!(url.path(), "" | "/") || url.query().is_some() || url.fragment().is_some() {
        bail!("{name} must be an origin without a path, like https://kiln.example.com, got {value:?}");
    }
    Ok(url.origin().ascii_serialization())
}

fn quantity(name: &str, default: &str) -> Result<String> {
    let value = var(name).unwrap_or_else(|| default.to_owned());
    let number = value.trim_end_matches(|c: char| c.is_ascii_alphabetic());
    let suffix = &value[number.len()..];
    let valid = !number.is_empty()
        && number.parse::<f64>().is_ok_and(|n| n > 0.0)
        && ["", "m", "k", "M", "G", "T", "Ki", "Mi", "Gi", "Ti"].contains(&suffix);
    if !valid {
        bail!("{name} must be a Kubernetes quantity like 2Gi or 500m, got {value:?}");
    }
    Ok(value)
}

fn labels(name: &str) -> Result<BTreeMap<String, String>> {
    list(name)
        .into_iter()
        .map(|pair| {
            pair.split_once('=')
                .map(|(k, v)| (k.to_owned(), v.to_owned()))
                .with_context(|| format!("{name} entries look like key=value, got {pair:?}"))
        })
        .collect()
}

impl Settings {
    pub fn listen() -> Result<SocketAddr> {
        var("KILN_LISTEN").unwrap_or_else(|| "0.0.0.0:8080".into()).parse().context("KILN_LISTEN")
    }

    pub fn public_url() -> Result<String> {
        origin("KILN_PUBLIC_URL", &required("KILN_PUBLIC_URL")?)
    }

    pub fn data_dir() -> PathBuf {
        PathBuf::from(var("KILN_DATA_DIR").unwrap_or_else(|| "/data".into()))
    }

    pub fn github_api() -> String {
        var("KILN_GITHUB_API").unwrap_or_else(|| "https://api.github.com".into())
    }

    pub fn github_web() -> String {
        var("KILN_GITHUB_WEB").unwrap_or_else(|| "https://github.com".into())
    }

    pub fn from_env(app: &GitHubApp) -> Result<Self> {
        let public_url = Self::public_url()?;
        let registry = var("KILN_REGISTRY")
            .unwrap_or_else(|| format!("ghcr.io/{}", app.owner_login.to_ascii_lowercase()))
            .trim_end_matches('/')
            .to_owned();
        if registry.contains("://") || !registry.contains('/') {
            bail!("KILN_REGISTRY is a registry and namespace without a scheme, like ghcr.io/example; got {registry:?}");
        }
        let registry_host = registry.split('/').next().unwrap_or_default().to_owned();
        let github_api = Self::github_api();
        let registry_password = app.registry_token.clone();
        let runner = kiln_runner::Settings {
            namespace: var("KILN_BUILD_NAMESPACE").unwrap_or_else(|| "kiln-builds".into()),
            kiln_image: var("KILN_IMAGE")
                .or_else(|| option_env!("KILN_BUILT_IMAGE").map(str::to_owned))
                .context("KILN_IMAGE is required when Kiln is not built by its release workflow")?,
            buildkit_image: var("KILN_BUILDKIT_IMAGE").unwrap_or_else(|| BUILDKIT_IMAGE.into()),
            buildkit_args: match var("KILN_BUILDKIT_ARGS") {
                Some(_) => list("KILN_BUILDKIT_ARGS"),
                None => vec!["--oci-worker-no-process-sandbox".into()],
            },
            registry: registry.clone(),
            registry_url: var("KILN_REGISTRY_URL").unwrap_or_else(|| format!("https://{registry_host}")),
            github_api: github_api.clone(),
            pull_secret: registry_password.is_some().then(|| PULL_SECRET.to_owned()),
            push_secret: PUSH_SECRET.into(),
            priority_class: var("KILN_PRIORITY_CLASS"),
            sandbox_runtime_class: var("KILN_SANDBOX_RUNTIME_CLASS"),
            avoid_labels: labels("KILN_AVOID_POD_LABELS")?,
            avoid_namespace: var("KILN_AVOID_POD_NAMESPACE"),
            workspace_size: quantity("KILN_WORKSPACE_SIZE", "8Gi")?,
            output_size: quantity("KILN_OUTPUT_SIZE", "10Gi")?,
            default_cpu: quantity("KILN_DEFAULT_CPU", "1")?,
            default_memory: quantity("KILN_DEFAULT_MEMORY", "2Gi")?,
            memory_limit_factor: number("KILN_MEMORY_LIMIT_FACTOR", 2)?.max(1),
        };
        Ok(Self {
            listen: Self::listen()?,
            public_url,
            data_dir: Self::data_dir(),
            github_web: Self::github_web(),
            github_api,
            app_id: app.app_id as u64,
            app_key: app.private_key.clone().into_bytes(),
            webhook_secrets: vec![app.webhook_secret.clone()],
            oauth_client_id: app.client_id.clone(),
            oauth_client_secret: app.client_secret.clone(),
            owners: vec![app.owner_login.clone()],
            include_public: var("KILN_INCLUDE_PUBLIC").is_some_and(|v| v == "true"),
            registry_url: runner.registry_url.clone(),
            registry,
            registry_username: app
                .registry_username
                .clone()
                .unwrap_or_else(|| if app.owner_is_user { app.owner_login.clone() } else { String::new() }),
            registry_password,
            slots: number("KILN_SLOTS", 2)?.max(1),
            poll_secs: number("KILN_POLL_SECONDS", 60)?.max(15),
            build_timeout_secs: number("KILN_BUILD_TIMEOUT_SECONDS", 1800)?.clamp(60, 7200),
            max_attempts: number("KILN_MAX_ATTEMPTS", 3)?.clamp(1, 10),
            retry_base_secs: number("KILN_RETRY_SECONDS", 30)?.clamp(0, 3600),
            release_window_secs: number("KILN_RELEASE_WINDOW_SECONDS", 1800)?.clamp(0, 86_400),
            repo_secret_prefix: var("KILN_REPO_SECRET_PREFIX").unwrap_or_else(|| "kiln-repo-".into()),
            cache_class: var("KILN_CACHE_STORAGE_CLASS"),
            cache_size: quantity("KILN_CACHE_SIZE", "20Gi")?,
            runner,
        })
    }

    pub fn local(data_dir: PathBuf, public_url: &str) -> Self {
        let runner = kiln_runner::Settings {
            namespace: "kiln-builds".into(),
            registry: "ghcr.io/kiln-demo".into(),
            ..kiln_runner::Settings::example()
        };
        Self {
            listen: "127.0.0.1:8080".parse().expect("valid address"),
            public_url: public_url.to_owned(),
            data_dir,
            github_api: "https://api.github.com".into(),
            github_web: "https://github.com".into(),
            app_id: 0,
            app_key: Vec::new(),
            webhook_secrets: Vec::new(),
            oauth_client_id: String::new(),
            oauth_client_secret: String::new(),
            owners: vec!["kiln-demo".into()],
            include_public: false,
            registry: runner.registry.clone(),
            registry_url: runner.registry_url.clone(),
            registry_username: "kiln-demo".into(),
            registry_password: Some("demo".into()),
            slots: 2,
            poll_secs: 15,
            build_timeout_secs: 1800,
            max_attempts: 3,
            retry_base_secs: 10,
            release_window_secs: 1800,
            repo_secret_prefix: "kiln-repo-".into(),
            cache_class: None,
            cache_size: "20Gi".into(),
            runner,
        }
    }

    pub fn registry_path(&self) -> &str {
        self.registry.split_once('/').map(|(_, path)| path).unwrap_or(&self.registry)
    }

    pub fn repository_for(&self, image: &str) -> String {
        format!("{}/{image}", self.registry_path())
    }

    pub fn repo_secret(&self, repo: &str) -> String {
        kiln_runner::resource_name(&self.repo_secret_prefix, repo)
    }
}
