use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};

#[derive(Debug, Clone)]
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
    pub allowed_users: Vec<u64>,
    pub owners: Vec<String>,
    pub include_public: bool,
    pub registry: String,
    pub registry_url: String,
    pub registry_username: String,
    pub registry_password: String,
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

fn secret(name: &str) -> Result<String> {
    let file = format!("{name}_FILE");
    if let Some(path) = var(&file) {
        return Ok(std::fs::read_to_string(&path).with_context(|| format!("reading {file}={path}"))?.trim().to_owned());
    }
    var(name).with_context(|| format!("{name} or {file} is required"))
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
    pub fn github_configured() -> bool {
        var("KILN_APP_ID").is_some()
    }

    pub fn listen() -> Result<SocketAddr> {
        var("KILN_LISTEN").unwrap_or_else(|| "0.0.0.0:8080".into()).parse().context("KILN_LISTEN")
    }

    pub fn public_url() -> Option<String> {
        var("KILN_PUBLIC_URL").and_then(|value| origin("KILN_PUBLIC_URL", &value).ok())
    }

    pub fn from_env() -> Result<Self> {
        let public_url = origin("KILN_PUBLIC_URL", &required("KILN_PUBLIC_URL")?)?;
        let registry = required("KILN_REGISTRY")?.trim_end_matches('/').to_owned();
        if registry.contains("://") || !registry.contains('/') {
            bail!("KILN_REGISTRY is a registry and namespace without a scheme, like ghcr.io/example; got {registry:?}");
        }
        let registry_host = registry.split('/').next().unwrap_or_default().to_owned();
        let github_api = var("KILN_GITHUB_API").unwrap_or_else(|| "https://api.github.com".into());
        let allowed_users = list("KILN_ALLOWED_USER_IDS")
            .into_iter()
            .map(|id| id.parse::<u64>().with_context(|| format!("KILN_ALLOWED_USER_IDS has a non-numeric id {id:?}")))
            .collect::<Result<Vec<_>>>()?;
        if allowed_users.is_empty() {
            bail!("KILN_ALLOWED_USER_IDS must list at least one numeric GitHub user id");
        }
        let owners = list("KILN_OWNERS");
        if owners.is_empty() {
            bail!("KILN_OWNERS must list the GitHub accounts whose repositories Kiln builds");
        }
        let mut webhook_secrets = vec![secret("KILN_WEBHOOK_SECRET")?];
        if var("KILN_WEBHOOK_SECRET_PREVIOUS").is_some() || var("KILN_WEBHOOK_SECRET_PREVIOUS_FILE").is_some() {
            webhook_secrets.push(secret("KILN_WEBHOOK_SECRET_PREVIOUS")?);
        }
        let runner = kiln_runner::Settings {
            namespace: var("KILN_BUILD_NAMESPACE").unwrap_or_else(|| "kiln-builds".into()),
            kiln_image: required("KILN_IMAGE")?,
            buildkit_image: required("KILN_BUILDKIT_IMAGE")?,
            buildkit_args: list("KILN_BUILDKIT_ARGS"),
            buildkit_rootless: var("KILN_BUILDKIT_ROOTLESS").is_none_or(|v| v != "false"),
            registry: registry.clone(),
            registry_url: var("KILN_REGISTRY_URL").unwrap_or_else(|| format!("https://{registry_host}")),
            github_api: github_api.clone(),
            pull_secret: var("KILN_PULL_SECRET"),
            push_secret: var("KILN_PUSH_SECRET").unwrap_or_else(|| "kiln-registry-push".into()),
            priority_class: var("KILN_PRIORITY_CLASS"),
            user_namespaces: var("KILN_USER_NAMESPACES").is_none_or(|v| v != "false"),
            avoid_labels: labels("KILN_AVOID_POD_LABELS")?,
            avoid_namespace: var("KILN_AVOID_POD_NAMESPACE"),
            workspace_size: quantity("KILN_WORKSPACE_SIZE", "8Gi")?,
            output_size: quantity("KILN_OUTPUT_SIZE", "10Gi")?,
            default_cpu: quantity("KILN_DEFAULT_CPU", "1")?,
            default_memory: quantity("KILN_DEFAULT_MEMORY", "2Gi")?,
            memory_limit_factor: number("KILN_MEMORY_LIMIT_FACTOR", 2)?.max(1),
        };
        if !runner.buildkit_rootless && !runner.user_namespaces {
            bail!(
                "KILN_BUILDKIT_ROOTLESS=false needs KILN_USER_NAMESPACES=true, otherwise BuildKit would run as root on the node"
            );
        }
        Ok(Self {
            listen: Self::listen()?,
            public_url,
            data_dir: PathBuf::from(var("KILN_DATA_DIR").unwrap_or_else(|| "/data".into())),
            github_web: var("KILN_GITHUB_WEB").unwrap_or_else(|| "https://github.com".into()),
            github_api,
            app_id: required("KILN_APP_ID")?.parse().context("KILN_APP_ID must be a number")?,
            app_key: secret("KILN_APP_PRIVATE_KEY")?.into_bytes(),
            webhook_secrets,
            oauth_client_id: required("KILN_OAUTH_CLIENT_ID")?,
            oauth_client_secret: secret("KILN_OAUTH_CLIENT_SECRET")?,
            allowed_users,
            owners,
            include_public: var("KILN_INCLUDE_PUBLIC").is_some_and(|v| v == "true"),
            registry_url: runner.registry_url.clone(),
            registry,
            registry_username: var("KILN_REGISTRY_USERNAME").unwrap_or_else(|| "kiln".into()),
            registry_password: secret("KILN_REGISTRY_PASSWORD")?,
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
            allowed_users: Vec::new(),
            owners: vec!["kiln-demo".into()],
            include_public: false,
            registry: runner.registry.clone(),
            registry_url: runner.registry_url.clone(),
            registry_username: String::new(),
            registry_password: String::new(),
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
