use std::collections::HashSet;
use std::fmt;
use std::path::{Component, Path};
use std::sync::LazyLock;

use indexmap::IndexMap;
use regex::Regex;
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer};

use crate::interpolate::{Vars, interpolate};

pub const FILE_NAME: &str = ".kiln.toml";
pub const MAX_FILE_BYTES: usize = 64 * 1024;
pub const MAX_TIMEOUT_SECS: u64 = 2 * 60 * 60;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub version: u32,
    #[serde(default)]
    pub on: Triggers,
    #[serde(default)]
    pub release: Release,
    #[serde(default)]
    pub images: Vec<Image>,
    #[serde(default)]
    pub test: IndexMap<String, Job>,
    #[serde(default)]
    pub verify: IndexMap<String, Job>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct Triggers {
    pub push: bool,
    pub tags: bool,
    pub pull_request: bool,
}

impl Default for Triggers {
    fn default() -> Self {
        Self { push: true, tags: true, pull_request: true }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct Release {
    pub mode: ReleaseMode,
    pub shadow: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ReleaseMode {
    #[default]
    Rc,
    Tags,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Image {
    pub name: String,
    #[serde(default = "default_dockerfile")]
    pub file: String,
    #[serde(default = "default_context")]
    pub context: String,
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default = "default_platforms")]
    pub platforms: Vec<String>,
    #[serde(default)]
    pub args: IndexMap<String, String>,
    #[serde(default)]
    pub secrets: IndexMap<String, String>,
    #[serde(default)]
    pub labels: IndexMap<String, String>,
    #[serde(default)]
    pub resources: Resources,
}

impl Image {
    pub fn named(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            file: default_dockerfile(),
            context: default_context(),
            target: None,
            platforms: default_platforms(),
            args: IndexMap::new(),
            secrets: IndexMap::new(),
            labels: IndexMap::new(),
            resources: Resources::default(),
        }
    }
}

fn default_dockerfile() -> String {
    "Dockerfile".to_owned()
}

fn default_context() -> String {
    ".".to_owned()
}

fn default_platforms() -> Vec<String> {
    vec!["linux/amd64".to_owned()]
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Job {
    pub image: String,
    pub steps: Vec<Step>,
    #[serde(default)]
    pub setup: Vec<Step>,
    #[serde(default)]
    pub env: IndexMap<String, String>,
    #[serde(default)]
    pub secrets: Vec<String>,
    #[serde(default)]
    pub cache: Vec<String>,
    #[serde(default)]
    pub services: Vec<Service>,
    #[serde(default)]
    pub timeout: Option<Timeout>,
    #[serde(default)]
    pub resources: Resources,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum Step {
    Command(String),
    Named { name: String, run: String },
}

impl Step {
    pub fn name(&self) -> &str {
        match self {
            Step::Command(run) => run.lines().next().unwrap_or_default(),
            Step::Named { name, .. } => name,
        }
    }

    pub fn run(&self) -> &str {
        match self {
            Step::Command(run) | Step::Named { run, .. } => run,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Service {
    pub name: String,
    #[serde(default)]
    pub image: Option<String>,
    #[serde(default)]
    pub candidate: Option<String>,
    #[serde(default)]
    pub env: IndexMap<String, String>,
    #[serde(default)]
    pub secrets: Vec<String>,
    #[serde(default)]
    pub user: Option<u32>,
    #[serde(default)]
    pub read_only: bool,
    #[serde(default)]
    pub tmpfs: Vec<String>,
    #[serde(default)]
    pub mounts: IndexMap<String, String>,
    #[serde(default)]
    pub expect_exit: Option<i32>,
    #[serde(default)]
    pub port: Option<u16>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Resources {
    #[serde(default)]
    pub cpu: Option<String>,
    #[serde(default)]
    pub memory: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, JsonSchema)]
#[schemars(with = "String")]
pub struct Timeout(pub u64);

impl<'de> Deserialize<'de> for Timeout {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        parse_duration(&text).map(Timeout).ok_or_else(|| {
            serde::de::Error::custom(format!("invalid duration {text:?}, expected e.g. \"90s\", \"20m\" or \"1h\""))
        })
    }
}

fn parse_duration(text: &str) -> Option<u64> {
    let text = text.trim();
    let split = text.find(|c: char| !c.is_ascii_digit())?;
    let (number, unit) = text.split_at(split);
    let number: u64 = number.parse().ok()?;
    let factor = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        _ => return None,
    };
    number.checked_mul(factor).filter(|secs| *secs > 0)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub path: String,
    pub line: Option<usize>,
    pub message: String,
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let place = match self.line {
            Some(line) => format!("{FILE_NAME}:{line}"),
            None => FILE_NAME.to_owned(),
        };
        if self.path.is_empty() {
            write!(f, "{place}: {}", self.message)
        } else {
            write!(f, "{place}: {}: {}", self.path, self.message)
        }
    }
}

pub fn parse(text: &str, repo: &str) -> Result<Config, Vec<Diagnostic>> {
    if text.len() > MAX_FILE_BYTES {
        return Err(vec![diagnostic("", format!("file is larger than {MAX_FILE_BYTES} bytes"))]);
    }
    let mut config: Config = toml::from_str(text).map_err(|error| {
        let line = error.span().map(|span| text[..span.start].matches('\n').count() + 1);
        vec![Diagnostic { path: String::new(), line, message: error.message().trim().to_owned() }]
    })?;
    if config.images.is_empty() {
        config.images.push(Image::named(&repo.to_ascii_lowercase()));
    }
    let diagnostics = validate(&config);
    if diagnostics.is_empty() { Ok(config) } else { Err(diagnostics) }
}

static IMAGE_NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[a-z0-9]+(?:[._-][a-z0-9]+)*$").unwrap());
static JOB_NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[a-z0-9][a-z0-9-]{0,40}$").unwrap());
static ENV_NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*$").unwrap());
static QUANTITY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[0-9]+(?:\.[0-9]+)?(?:m|Ki|Mi|Gi)?$").unwrap());

pub fn is_quantity(value: &str) -> bool {
    QUANTITY.is_match(value)
}

const RESERVED_PATHS: [&str; 6] = ["/workspace", "/kiln", "/proc", "/sys", "/dev", "/etc/resolv.conf"];

fn diagnostic(path: impl Into<String>, message: impl Into<String>) -> Diagnostic {
    Diagnostic { path: path.into(), line: None, message: message.into() }
}

fn validate(config: &Config) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    if config.version != 1 {
        out.push(diagnostic("version", format!("unsupported version {}, only 1 is supported", config.version)));
    }
    if config.release.mode == ReleaseMode::Tags && !config.on.tags {
        out.push(diagnostic(
            "release.mode",
            "\"tags\" releases need on.tags = true, otherwise nothing is ever released",
        ));
    }
    let mut image_names = HashSet::new();
    for (index, image) in config.images.iter().enumerate() {
        let at = format!("images[{index}]");
        if !IMAGE_NAME.is_match(&image.name) {
            out.push(diagnostic(format!("{at}.name"), format!("{:?} is not a valid image name", image.name)));
        }
        if !image_names.insert(image.name.as_str()) {
            out.push(diagnostic(format!("{at}.name"), format!("image {:?} is listed twice", image.name)));
        }
        for (field, value) in [("file", &image.file), ("context", &image.context)] {
            if let Err(message) = check_relative(value) {
                out.push(diagnostic(format!("{at}.{field}"), message));
            }
        }
        for (name, value) in [("cpu", &image.resources.cpu), ("memory", &image.resources.memory)] {
            if let Some(value) = value
                && !QUANTITY.is_match(value)
            {
                out.push(diagnostic(
                    format!("{at}.resources.{name}"),
                    format!("{value:?} is not a Kubernetes quantity"),
                ));
            }
        }
        if image.platforms.is_empty() {
            out.push(diagnostic(format!("{at}.platforms"), "at least one platform is required"));
        }
        for (key, value) in image.args.iter().chain(&image.labels) {
            if let Err(message) = interpolate(value, &Vars::placeholder()) {
                out.push(diagnostic(format!("{at}.{key}"), message));
            }
        }
        for (id, key) in &image.secrets {
            if !ENV_NAME.is_match(id) || !ENV_NAME.is_match(key) {
                out.push(diagnostic(
                    format!("{at}.secrets.{id}"),
                    "secret ids and keys are letters, digits and underscores",
                ));
            }
        }
    }
    let mut job_names = HashSet::new();
    for (phase, jobs) in [("test", &config.test), ("verify", &config.verify)] {
        for (name, job) in jobs {
            let at = format!("{phase}.{name}");
            if !JOB_NAME.is_match(name) {
                out.push(diagnostic(&at, "job names are lowercase letters, digits and dashes"));
            }
            if !job_names.insert(name.as_str()) {
                out.push(diagnostic(&at, format!("job {name:?} exists in both test and verify")));
            }
            validate_job(&at, job, phase == "verify", &image_names, &mut out);
        }
    }
    out
}

fn validate_job(at: &str, job: &Job, verify: bool, images: &HashSet<&str>, out: &mut Vec<Diagnostic>) {
    if job.image.trim().is_empty() {
        out.push(diagnostic(format!("{at}.image"), "an image is required"));
    }
    if job.steps.is_empty() {
        out.push(diagnostic(format!("{at}.steps"), "at least one step is required"));
    }
    for (index, step) in job.steps.iter().chain(&job.setup).enumerate() {
        if step.run().trim().is_empty() {
            out.push(diagnostic(format!("{at}.steps[{index}]"), "a step needs a command"));
        }
    }
    if let Some(Timeout(secs)) = job.timeout
        && secs > MAX_TIMEOUT_SECS
    {
        out.push(diagnostic(format!("{at}.timeout"), "timeouts are capped at 2h"));
    }
    for (name, value) in [("cpu", &job.resources.cpu), ("memory", &job.resources.memory)] {
        if let Some(value) = value
            && !QUANTITY.is_match(value)
        {
            out.push(diagnostic(format!("{at}.resources.{name}"), format!("{value:?} is not a Kubernetes quantity")));
        }
    }
    for key in job.env.keys() {
        if !ENV_NAME.is_match(key) {
            out.push(diagnostic(format!("{at}.env.{key}"), "environment names are letters, digits and underscores"));
        }
    }
    for name in &job.secrets {
        if !ENV_NAME.is_match(name) {
            out.push(diagnostic(
                format!("{at}.secrets"),
                format!("{name:?} is not a valid secret key; use letters, digits and underscores"),
            ));
        }
    }
    let mut caches = HashSet::new();
    for path in &job.cache {
        if Path::new(path).components().any(|c| matches!(c, Component::ParentDir)) {
            out.push(diagnostic(format!("{at}.cache"), format!("{path:?} must not contain ..")));
            continue;
        }
        let normal = normalize(path);
        let reserved = RESERVED_PATHS.iter().any(|r| normal == *r || normal.starts_with(&format!("{r}/")));
        if matches!(normal.as_str(), "" | "/") || reserved {
            out.push(diagnostic(
                format!("{at}.cache"),
                format!("{path:?} is used by Kiln or the system and cannot be a cache"),
            ));
        } else if !caches.insert(normal) {
            out.push(diagnostic(format!("{at}.cache"), format!("{path:?} is listed twice")));
        }
    }
    let mut uses_candidate = false;
    let mut service_names = HashSet::new();
    for (index, service) in job.services.iter().enumerate() {
        let here = format!("{at}.services[{index}]");
        if !JOB_NAME.is_match(&service.name) {
            out.push(diagnostic(format!("{here}.name"), "service names are lowercase letters, digits and dashes"));
        } else if !service_names.insert(service.name.as_str()) {
            out.push(diagnostic(format!("{here}.name"), format!("service {:?} is listed twice", service.name)));
        }
        if service.user == Some(0) {
            out.push(diagnostic(format!("{here}.user"), "services run as a non-root user; pick a uid above 0"));
        }
        for key in service.env.keys() {
            if !ENV_NAME.is_match(key) {
                out.push(diagnostic(
                    format!("{here}.env.{key}"),
                    "environment names are letters, digits and underscores",
                ));
            }
        }
        for name in &service.secrets {
            if !ENV_NAME.is_match(name) {
                out.push(diagnostic(
                    format!("{here}.secrets"),
                    format!("{name:?} is not a valid secret key; use letters, digits and underscores"),
                ));
            }
        }
        match (&service.image, &service.candidate) {
            (Some(_), None) => {}
            (None, Some(candidate)) => {
                uses_candidate = true;
                if !verify {
                    out.push(diagnostic(&here, "candidate services are only available in verify jobs"));
                } else if !images.contains(candidate.as_str()) {
                    out.push(diagnostic(&here, format!("candidate {candidate:?} is not one of the images")));
                }
            }
            _ => out.push(diagnostic(&here, "set exactly one of image or candidate")),
        }
        for target in service.mounts.values() {
            if let Err(message) = check_relative(target) {
                out.push(diagnostic(format!("{here}.mounts"), message));
            }
        }
    }
    if verify && !uses_candidate {
        out.push(diagnostic(at, "a verify job needs a candidate service; without one it belongs under [test]"));
    }
}

fn normalize(path: &str) -> String {
    let parts: Vec<String> = Path::new(path)
        .components()
        .filter_map(|c| match c {
            Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    if path.starts_with('/') { format!("/{}", parts.join("/")) } else { parts.join("/") }
}

fn check_relative(value: &str) -> Result<(), String> {
    let path = Path::new(value);
    if path.is_absolute() {
        return Err(format!("{value:?} must be relative to the repository root"));
    }
    if path.components().any(|component| matches!(component, Component::ParentDir | Component::Prefix(_))) {
        return Err(format!("{value:?} must stay inside the repository"));
    }
    Ok(())
}

pub fn json_schema() -> serde_json::Value {
    serde_json::to_value(schemars::schema_for!(Config)).expect("schema serializes")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn messages(text: &str) -> Vec<String> {
        parse(text, "app").unwrap_err().into_iter().map(|d| format!("{}: {}", d.path, d.message)).collect()
    }

    #[test]
    fn minimal_config_builds_the_repo_image() {
        let config = parse("version = 1", "WebShop").unwrap();
        assert_eq!(config.images, vec![Image::named("webshop")]);
        assert!(config.on.push && config.on.tags && config.on.pull_request);
    }

    #[test]
    fn build_paths_cannot_escape_the_repository() {
        let errors =
            messages("version = 1\n[[images]]\nname = \"app\"\nfile = \"../secrets/Dockerfile\"\ncontext = \"/etc\"\n");
        assert_eq!(errors.len(), 2, "{errors:?}");
        assert!(errors.iter().any(|e| e.starts_with("images[0].file")));
        assert!(errors.iter().any(|e| e.starts_with("images[0].context")));
    }

    #[test]
    fn caches_cannot_cover_the_workspace_or_kilns_own_paths() {
        for path in ["/./kiln", "//dev/shm", "/workspace/.", "./.", "/"] {
            let text = format!("version = 1\n[test.unit]\nimage = \"node\"\nsteps = [\"true\"]\ncache = [{path:?}]\n");
            assert!(parse(&text, "app").is_err(), "{path}");
        }
        let text = "version = 1\n[test.unit]\nimage = \"node\"\nsteps = [\"true\"]\ncache = [\"/root/.npm\", \"node_modules\"]\n";
        assert!(parse(text, "app").is_ok());
    }

    #[test]
    fn unknown_keys_are_reported_with_their_line() {
        let error = parse("version = 1\n\n[release]\nmodee = \"rc\"\n", "app").unwrap_err();
        assert_eq!(error[0].line, Some(4));
        assert!(error[0].message.contains("modee"), "{}", error[0].message);
    }

    #[test]
    fn candidates_only_run_in_verify_against_known_images() {
        let errors = messages(
            "version = 1\n[test.unit]\nimage = \"alpine\"\nsteps = [\"true\"]\nservices = [{ name = \"app\", candidate = \"app\" }]\n[verify.smoke]\nimage = \"alpine\"\nsteps = [\"true\"]\nservices = [{ name = \"app\", candidate = \"other\" }]\n",
        );
        assert!(errors.iter().any(|e| e.contains("only available in verify")), "{errors:?}");
        assert!(errors.iter().any(|e| e.contains("\"other\" is not one of the images")), "{errors:?}");
    }
}
