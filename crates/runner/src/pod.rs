use std::collections::BTreeMap;

use k8s_openapi::api::core::v1::Pod;
use kiln_core::config::{Config, Job, Resources, Service};
use kiln_core::interpolate::{Vars, interpolate};
use kiln_store::{TaskKind, TaskPlan};
use serde::Serialize;
use serde_json::{Value, json};

use crate::{ATTEMPT_LABEL, BUILD_LABEL, MANAGED_BY, REPO_LABEL, label_value, pod_name};

const WORKSPACE: &str = "/workspace";
const KILN_BIN: &str = "/kiln/bin";
const OUT: &str = "/kiln/out";
const BUILDKIT_SOCKET: &str = "/run/buildkit";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub namespace: String,
    pub kiln_image: String,
    pub buildkit_image: String,
    pub buildkit_args: Vec<String>,
    pub registry: String,
    pub registry_url: String,
    pub github_api: String,
    pub pull_secret: Option<String>,
    pub push_secret: String,
    pub priority_class: Option<String>,
    pub user_namespaces: bool,
    pub avoid_labels: BTreeMap<String, String>,
    pub avoid_namespace: Option<String>,
    pub workspace_size: String,
    pub output_size: String,
    pub default_cpu: String,
    pub default_memory: String,
    pub memory_limit_factor: u32,
}

impl Settings {
    pub fn example() -> Self {
        Self {
            namespace: "kiln-builds".into(),
            kiln_image: "ghcr.io/example/kiln@sha256:1111111111111111111111111111111111111111111111111111111111111111"
                .into(),
            buildkit_image: "moby/buildkit@sha256:2222222222222222222222222222222222222222222222222222222222222222"
                .into(),
            buildkit_args: vec!["--oci-worker-no-process-sandbox".into()],
            registry: "ghcr.io/example".into(),
            registry_url: "https://ghcr.io".into(),
            github_api: "https://api.github.com".into(),
            pull_secret: Some("registry-pull".into()),
            push_secret: "registry-push".into(),
            priority_class: Some("kiln-build".into()),
            user_namespaces: true,
            avoid_labels: BTreeMap::from([("app.kubernetes.io/name".into(), "kiln".into())]),
            avoid_namespace: Some("kiln".into()),
            workspace_size: "8Gi".into(),
            output_size: "10Gi".into(),
            default_cpu: "1".into(),
            default_memory: "2Gi".into(),
            memory_limit_factor: 2,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    TestsOnly,
    Full,
}

#[derive(Debug, Clone)]
pub struct Plan<'a> {
    pub build_id: i64,
    pub attempt: i64,
    pub repo: &'a str,
    pub sha: &'a str,
    pub reference: &'a str,
    pub event: &'a str,
    pub version: Option<&'a str>,
    pub run: i64,
    pub config: &'a Config,
    pub scope: Scope,
    pub trusted: bool,
    pub fetch_secret: &'a str,
    pub repo_secret: Option<&'a str>,
    pub cache_claim: Option<&'a str>,
    pub timeout_secs: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StepSpec {
    pub name: String,
    pub run: String,
}

pub struct Rendered {
    pub pod: Pod,
    pub tasks: Vec<TaskPlan>,
}

pub fn cache_claim_name(repo: &str) -> String {
    let label = label_value(&repo.replace('/', "-"));
    format!("kiln-cache-{}", &label[..label.len().min(48)])
}

pub fn candidate_ref(settings: &Settings, image: &str, sha: &str) -> String {
    format!("{}/{image}:sha-{sha}", settings.registry)
}

struct Builder<'a> {
    plan: &'a Plan<'a>,
    settings: &'a Settings,
    init: Vec<Value>,
    tasks: Vec<TaskPlan>,
    volumes: Vec<Value>,
    secret_items: Vec<String>,
}

pub fn render(plan: &Plan<'_>, settings: &Settings) -> Result<Rendered, String> {
    let mut builder =
        Builder { plan, settings, init: Vec::new(), tasks: Vec::new(), volumes: Vec::new(), secret_items: Vec::new() };
    builder.fetch();
    for (name, job) in &plan.config.test {
        builder.job("test", name, job)?;
    }
    if plan.scope == Scope::Full && !plan.config.images.is_empty() {
        builder.images()?;
        builder.push();
        for (name, job) in &plan.config.verify {
            builder.job("verify", name, job)?;
        }
    }
    builder.finish()
}

impl Builder<'_> {
    fn task(&mut self, kind: TaskKind, name: &str, container: &str, steps: Vec<String>) {
        self.tasks.push(TaskPlan { kind, name: name.to_owned(), container: container.to_owned(), steps });
    }

    fn base_env(&self) -> Vec<Value> {
        let mut env = vec![
            json!({ "name": "CI", "value": "true" }),
            json!({ "name": "KILN", "value": "true" }),
            json!({ "name": "KILN_REPOSITORY", "value": self.plan.repo }),
            json!({ "name": "KILN_SHA", "value": self.plan.sha }),
            json!({ "name": "KILN_REF", "value": self.plan.reference }),
            json!({ "name": "KILN_EVENT", "value": self.plan.event }),
            json!({ "name": "KILN_RUN", "value": self.plan.run.to_string() }),
        ];
        if let Some(version) = self.plan.version {
            env.push(json!({ "name": "KILN_VERSION", "value": version }));
        }
        env
    }

    fn workspace_mount(&self) -> Value {
        json!({ "name": "workspace", "mountPath": WORKSPACE })
    }

    fn fetch(&mut self) {
        self.volumes.push(json!({ "name": "workspace", "emptyDir": { "sizeLimit": self.settings.workspace_size } }));
        self.volumes.push(json!({ "name": "kiln-bin", "emptyDir": { "sizeLimit": "256Mi" } }));
        self.volumes.push(
            json!({ "name": "fetch-token", "secret": { "secretName": self.plan.fetch_secret, "defaultMode": 0o440 } }),
        );
        self.init.push(json!({
            "name": "fetch",
            "image": self.settings.kiln_image,
            "args": ["helper", "fetch", "--repo", self.plan.repo, "--sha", self.plan.sha, "--dest", WORKSPACE,
                     "--install", KILN_BIN, "--api", self.settings.github_api, "--token-file", "/kiln/token/token"],
            "volumeMounts": [
                self.workspace_mount(),
                { "name": "kiln-bin", "mountPath": KILN_BIN },
                { "name": "fetch-token", "mountPath": "/kiln/token", "readOnly": true },
            ],
            "resources": { "requests": { "cpu": "100m", "memory": "128Mi" }, "limits": { "memory": "512Mi" } },
            "securityContext": restricted(),
        }));
        self.task(TaskKind::Fetch, "fetch", "fetch", Vec::new());
    }

    fn job(&mut self, phase: &str, name: &str, job: &Job) -> Result<(), String> {
        let container = format!("{phase}-{name}");
        let kind = if phase == "test" { TaskKind::Test } else { TaskKind::Verify };
        let mut mounts =
            vec![self.workspace_mount(), json!({ "name": "kiln-bin", "mountPath": KILN_BIN, "readOnly": true })];
        mounts.extend(self.cache_mounts(name, job));
        let mut env = self.base_env();
        for (key, value) in &job.env {
            env.push(json!({ "name": key, "value": value }));
        }
        if self.plan.trusted && !job.secrets.is_empty() {
            for key in &job.secrets {
                env.push(self.secret_env(key, key)?);
            }
            env.push(json!({ "name": "KILN_SECRET_NAMES", "value": job.secrets.join(",") }));
        }
        if phase == "verify" {
            for image in &self.plan.config.images {
                let env_name = format!("KILN_IMAGE_{}", image.name.to_ascii_uppercase().replace(['-', '.'], "_"));
                env.push(
                    json!({ "name": env_name, "value": candidate_ref(self.settings, &image.name, self.plan.sha) }),
                );
            }
        }
        if !job.setup.is_empty() {
            self.init.push(self.steps_container(
                &format!("{container}-setup"),
                &job.image,
                &job.setup,
                &env,
                &mounts,
                &job.resources,
                job_timeout(job, self.plan.timeout_secs),
            )?);
            self.task(
                kind,
                &format!("{name} setup"),
                &format!("{container}-setup"),
                job.setup.iter().map(|s| s.name().to_owned()).collect(),
            );
        }
        for (index, service) in job.services.iter().enumerate() {
            let sidecar = self.service(&format!("{container}-svc{index}"), service)?;
            self.init.push(sidecar);
        }
        self.init.push(self.steps_container(
            &container,
            &job.image,
            &job.steps,
            &env,
            &mounts,
            &job.resources,
            job_timeout(job, self.plan.timeout_secs),
        )?);
        self.task(kind, name, &container, job.steps.iter().map(|s| s.name().to_owned()).collect());
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn steps_container(
        &self,
        name: &str,
        image: &str,
        steps: &[kiln_core::config::Step],
        env: &[Value],
        mounts: &[Value],
        resources: &Resources,
        timeout: u64,
    ) -> Result<Value, String> {
        let specs: Vec<StepSpec> =
            steps.iter().map(|s| StepSpec { name: s.name().to_owned(), run: s.run().to_owned() }).collect();
        let mut env = env.to_vec();
        env.push(json!({ "name": "KILN_STEPS", "value": serde_json::to_string(&specs).map_err(|e| e.to_string())? }));
        env.push(json!({ "name": "KILN_TIMEOUT", "value": timeout.to_string() }));
        Ok(json!({
            "name": name,
            "image": image,
            "command": [format!("{KILN_BIN}/kiln"), "helper", "steps"],
            "workingDir": WORKSPACE,
            "env": env,
            "volumeMounts": mounts,
            "resources": self.resources(resources),
            "securityContext": { "allowPrivilegeEscalation": false, "capabilities": { "drop": ["NET_RAW"] } },
        }))
    }

    fn service(&mut self, name: &str, service: &Service) -> Result<Value, String> {
        let image = match (&service.image, &service.candidate) {
            (Some(image), _) => image.clone(),
            (None, Some(candidate)) => candidate_ref(self.settings, candidate, self.plan.sha),
            (None, None) => return Err(format!("service {} has no image", service.name)),
        };
        let mut env: Vec<Value> = service.env.iter().map(|(k, v)| json!({ "name": k, "value": v })).collect();
        if self.plan.trusted {
            for key in &service.secrets {
                env.push(self.secret_env(key, key)?);
            }
        }
        let mut mounts = Vec::new();
        for (index, path) in service.tmpfs.iter().enumerate() {
            let volume = format!("{name}-tmp{index}");
            self.volumes.push(json!({ "name": volume, "emptyDir": { "medium": "Memory", "sizeLimit": "256Mi" } }));
            mounts.push(json!({ "name": volume, "mountPath": path }));
        }
        for (target, source) in &service.mounts {
            mounts.push(json!({ "name": "workspace", "mountPath": target, "subPath": source.trim_start_matches("./"), "readOnly": true }));
        }
        let mut security = restricted();
        if let Some(user) = service.user {
            security["runAsUser"] = json!(user);
            security["runAsGroup"] = json!(user);
        }
        security["readOnlyRootFilesystem"] = json!(service.read_only);
        Ok(json!({
            "name": name,
            "image": image,
            "imagePullPolicy": if service.candidate.is_some() { "Always" } else { "IfNotPresent" },
            "restartPolicy": "Always",
            "env": env,
            "volumeMounts": mounts,
            "resources": { "requests": { "cpu": "100m", "memory": "128Mi" }, "limits": { "memory": "1Gi" } },
            "securityContext": security,
        }))
    }

    fn images(&mut self) -> Result<(), String> {
        if !self.plan.trusted {
            return Err("image builds only run for trusted builds".into());
        }
        self.volumes.push(json!({ "name": "buildkit-run", "emptyDir": {} }));
        self.volumes.push(json!({ "name": "out", "emptyDir": { "sizeLimit": self.settings.output_size } }));
        let mut args =
            vec![format!("--addr=unix://{BUILDKIT_SOCKET}/buildkitd.sock"), "--oci-worker-net=host".to_owned()];
        args.extend(self.settings.buildkit_args.iter().cloned());
        let mut buildkit_mounts = vec![json!({ "name": "buildkit-run", "mountPath": BUILDKIT_SOCKET })];
        if self.plan.cache_claim.is_some() {
            buildkit_mounts.push(
                json!({ "name": "cache", "mountPath": "/home/user/.local/share/buildkit", "subPath": "buildkit" }),
            );
        }
        self.init.push(json!({
            "name": "buildkitd",
            "image": self.settings.buildkit_image,
            "restartPolicy": "Always",
            "args": args,
            "env": [{ "name": "BUILDKITD_FLAGS", "value": "" }],
            "volumeMounts": buildkit_mounts,
            "startupProbe": {
                "exec": { "command": ["buildctl", format!("--addr=unix://{BUILDKIT_SOCKET}/buildkitd.sock"), "debug", "workers"] },
                "periodSeconds": 2,
                "failureThreshold": 60,
            },
            "resources": self.resources(&Resources { cpu: None, memory: None }),
            "securityContext": {
                "runAsUser": 1000,
                "runAsGroup": 1000,
                "seccompProfile": { "type": "Unconfined" },
                "appArmorProfile": { "type": "Unconfined" },
            },
        }));
        let vars = Vars {
            sha: self.plan.sha.to_owned(),
            reference: self.plan.reference.to_owned(),
            version: self.plan.version.unwrap_or("0.0.0-dev").to_owned(),
            run: self.plan.run.max(0) as u64,
        };
        let images = self.plan.config.images.clone();
        for (index, image) in images.iter().enumerate() {
            let container = format!("image-{index}");
            let file = std::path::Path::new(&image.file);
            let dockerfile_dir = file
                .parent()
                .map(|p| p.to_string_lossy().into_owned())
                .filter(|p| !p.is_empty())
                .unwrap_or_else(|| ".".into());
            let filename =
                file.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_else(|| "Dockerfile".into());
            let mut args = vec![
                format!("--addr=unix://{BUILDKIT_SOCKET}/buildkitd.sock"),
                "build".into(),
                "--progress=plain".into(),
                "--frontend=dockerfile.v0".into(),
                format!("--local=context={WORKSPACE}/{}", image.context),
                format!("--local=dockerfile={WORKSPACE}/{dockerfile_dir}"),
                format!("--opt=filename={filename}"),
                format!("--opt=platform={}", image.platforms.join(",")),
                "--opt=attest:provenance=mode=max".into(),
            ];
            if let Some(target) = &image.target {
                args.push(format!("--opt=target={target}"));
            }
            for (key, value) in &image.args {
                args.push(format!("--opt=build-arg:{key}={}", interpolate(value, &vars)?));
            }
            let mut labels = BTreeMap::from([
                ("org.opencontainers.image.source".to_owned(), format!("https://github.com/{}", self.plan.repo)),
                ("org.opencontainers.image.revision".to_owned(), self.plan.sha.to_owned()),
            ]);
            if let Some(version) = self.plan.version {
                labels.insert("org.opencontainers.image.version".into(), version.to_owned());
            }
            for (key, value) in &image.labels {
                labels.insert(key.clone(), interpolate(value, &vars)?);
            }
            for (key, value) in &labels {
                args.push(format!("--opt=label:{key}={value}"));
            }
            let mut mounts = vec![
                json!({ "name": "workspace", "mountPath": WORKSPACE, "readOnly": true }),
                json!({ "name": "kiln-bin", "mountPath": KILN_BIN, "readOnly": true }),
                json!({ "name": "buildkit-run", "mountPath": BUILDKIT_SOCKET }),
                json!({ "name": "out", "mountPath": OUT }),
            ];
            if !image.secrets.is_empty() {
                for key in image.secrets.values() {
                    if !self.secret_items.contains(key) {
                        self.secret_items.push(key.clone());
                    }
                }
                mounts.push(json!({ "name": "build-secrets", "mountPath": "/kiln/secrets", "readOnly": true }));
                for (id, key) in &image.secrets {
                    args.push(format!("--secret=id={id},src=/kiln/secrets/{key}"));
                }
            }
            args.push(format!("--output=type=oci,dest={OUT}/{index},tar=false"));
            let mut command = vec![format!("{KILN_BIN}/kiln"), "helper".into(), "exec".into()];
            if !image.secrets.is_empty() {
                command.push("--redact-dir=/kiln/secrets".into());
            }
            command.extend(["--".into(), "buildctl".into()]);
            self.init.push(json!({
                "name": container,
                "image": self.settings.buildkit_image,
                "command": command,
                "args": args,
                "volumeMounts": mounts,
                "resources": { "requests": { "cpu": "100m", "memory": "128Mi" }, "limits": { "memory": "512Mi" } },
                "securityContext": restricted(),
            }));
            self.task(TaskKind::Image, &image.name, &container, Vec::new());
        }
        if !self.secret_items.is_empty() {
            let secret = self.plan.repo_secret.ok_or("images use secrets but the repository has no Kiln secret")?;
            let items: Vec<Value> = self.secret_items.iter().map(|key| json!({ "key": key, "path": key })).collect();
            self.volumes.push(json!({ "name": "build-secrets", "secret": { "secretName": secret, "items": items, "defaultMode": 0o444 } }));
        }
        Ok(())
    }

    fn push(&mut self) {
        let mut args = vec![
            "helper".to_owned(),
            "push".into(),
            "--registry".into(),
            self.settings.registry_url.clone(),
            "--tag".into(),
            format!("sha-{}", self.plan.sha),
        ];
        for (index, image) in self.plan.config.images.iter().enumerate() {
            let repository = format!("{}/{}", self.settings.registry, image.name);
            let repository = repository.split_once('/').map(|(_, path)| path.to_owned()).unwrap_or(repository);
            args.push("--image".into());
            args.push(format!("{}={repository}={OUT}/{index}", image.name));
        }
        self.init.push(json!({
            "name": "push",
            "image": self.settings.kiln_image,
            "args": args,
            "env": [
                { "name": "KILN_REGISTRY_USERNAME", "valueFrom": { "secretKeyRef": { "name": self.settings.push_secret, "key": "username" } } },
                { "name": "KILN_REGISTRY_PASSWORD", "valueFrom": { "secretKeyRef": { "name": self.settings.push_secret, "key": "password" } } },
            ],
            "volumeMounts": [{ "name": "out", "mountPath": OUT, "readOnly": true }],
            "resources": { "requests": { "cpu": "200m", "memory": "128Mi" }, "limits": { "memory": "512Mi" } },
            "securityContext": restricted(),
        }));
        self.task(TaskKind::Push, "push", "push", Vec::new());
    }

    fn cache_mounts(&self, job: &str, spec: &Job) -> Vec<Value> {
        let Some(_) = self.plan.cache_claim else { return Vec::new() };
        let lane = if self.plan.trusted { "steps" } else { "steps-pr" };
        spec.cache
            .iter()
            .map(|path| {
                let key = label_value(&path.replace('/', "-"));
                json!({ "name": "cache", "mountPath": absolute(path), "subPath": format!("{lane}/{job}/{key}") })
            })
            .collect()
    }

    fn secret_env(&self, env: &str, key: &str) -> Result<Value, String> {
        let secret = self.plan.repo_secret.ok_or_else(|| format!("{key} needs a Kiln secret for this repository"))?;
        Ok(json!({ "name": env, "valueFrom": { "secretKeyRef": { "name": secret, "key": key } } }))
    }

    fn resources(&self, resources: &Resources) -> Value {
        let cpu = resources.cpu.clone().unwrap_or_else(|| self.settings.default_cpu.clone());
        let memory = resources.memory.clone().unwrap_or_else(|| self.settings.default_memory.clone());
        let limit = scale_quantity(&memory, self.settings.memory_limit_factor).unwrap_or_else(|| memory.clone());
        json!({ "requests": { "cpu": cpu, "memory": memory, "ephemeral-storage": "1Gi" }, "limits": { "memory": limit } })
    }

    fn finish(mut self) -> Result<Rendered, String> {
        if let Some(claim) = self.plan.cache_claim {
            self.volumes.push(json!({ "name": "cache", "persistentVolumeClaim": { "claimName": claim } }));
        }
        let name = pod_name(self.plan.build_id, self.plan.attempt);
        let mut affinity = json!({
            "nodeAffinity": { "requiredDuringSchedulingIgnoredDuringExecution": { "nodeSelectorTerms": [
                { "matchExpressions": [{ "key": "node-role.kubernetes.io/control-plane", "operator": "DoesNotExist" }] }
            ] } },
            "podAntiAffinity": { "preferredDuringSchedulingIgnoredDuringExecution": [{
                "weight": 50,
                "podAffinityTerm": { "topologyKey": "kubernetes.io/hostname", "labelSelector": { "matchLabels": { MANAGED_BY: "kiln" } } },
            }] },
        });
        if !self.settings.avoid_labels.is_empty() {
            let mut term = json!({ "topologyKey": "kubernetes.io/hostname", "labelSelector": { "matchLabels": self.settings.avoid_labels } });
            if let Some(namespace) = &self.settings.avoid_namespace {
                term["namespaces"] = json!([namespace]);
            }
            affinity["podAntiAffinity"]["requiredDuringSchedulingIgnoredDuringExecution"] = json!([term]);
        }
        let mut spec = json!({
            "restartPolicy": "Never",
            "activeDeadlineSeconds": self.plan.timeout_secs,
            "terminationGracePeriodSeconds": 10,
            "automountServiceAccountToken": false,
            "enableServiceLinks": false,
            "securityContext": { "seccompProfile": { "type": "RuntimeDefault" }, "fsGroup": 1000, "fsGroupChangePolicy": "OnRootMismatch" },
            "affinity": affinity,
            "initContainers": self.init,
            "containers": [{
                "name": "done",
                "image": self.settings.kiln_image,
                "args": ["helper", "done"],
                "resources": { "requests": { "cpu": "10m", "memory": "16Mi" }, "limits": { "memory": "64Mi" } },
                "securityContext": restricted(),
            }],
            "volumes": self.volumes,
        });
        if self.settings.user_namespaces {
            spec["hostUsers"] = json!(false);
        }
        if let Some(class) = &self.settings.priority_class {
            spec["priorityClassName"] = json!(class);
        }
        if let Some(secret) = &self.settings.pull_secret {
            spec["imagePullSecrets"] = json!([{ "name": secret }]);
        }
        let pod = json!({
            "apiVersion": "v1",
            "kind": "Pod",
            "metadata": {
                "name": name,
                "namespace": self.settings.namespace,
                "labels": {
                    MANAGED_BY: "kiln",
                    BUILD_LABEL: self.plan.build_id.to_string(),
                    ATTEMPT_LABEL: self.plan.attempt.to_string(),
                    REPO_LABEL: label_value(&self.plan.repo.replace('/', ".")),
                },
                "annotations": { "kiln.dev/sha": self.plan.sha, "kiln.dev/ref": self.plan.reference },
            },
            "spec": spec,
        });
        let pod: Pod = serde_json::from_value(pod).map_err(|e| format!("pod spec is invalid: {e}"))?;
        Ok(Rendered { pod, tasks: self.tasks })
    }
}

fn restricted() -> Value {
    json!({
        "runAsNonRoot": true,
        "runAsUser": 65532,
        "runAsGroup": 65532,
        "allowPrivilegeEscalation": false,
        "readOnlyRootFilesystem": true,
        "capabilities": { "drop": ["ALL"] },
    })
}

fn absolute(path: &str) -> String {
    if path.starts_with('/') { path.to_owned() } else { format!("{WORKSPACE}/{}", path.trim_start_matches("./")) }
}

fn job_timeout(job: &Job, build_timeout: u64) -> u64 {
    job.timeout.map(|t| t.0).unwrap_or(build_timeout).min(build_timeout)
}

fn scale_quantity(quantity: &str, factor: u32) -> Option<String> {
    let split = quantity.find(|c: char| !c.is_ascii_digit())?;
    let (number, unit) = quantity.split_at(split);
    let number: u64 = number.parse().ok()?;
    Some(format!("{}{unit}", number * u64::from(factor)))
}
