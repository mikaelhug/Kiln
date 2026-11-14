use kiln_core::config::{Config, parse};
use kiln_runner::k8s_openapi::api::core::v1::{Container, Pod};
use kiln_runner::{Plan, Scope, Settings, render};
use kiln_store::TaskKind;

const CONFIG: &str = r#"
version = 1

[[images]]
name = "web"
args = { DEPLOYMENT_ID = "${sha}" }
secrets = { actions_key = "ACTIONS_KEY" }

[test.unit]
image = "node:26"
cache = ["/root/.npm"]
secrets = ["NPM_TOKEN"]
steps = ["npm ci", "npm test"]

[verify.smoke]
image = "curlimages/curl:8"
steps = ["curl -f http://127.0.0.1:8080/healthz"]
services = [{ name = "app", candidate = "web", user = 10001, read_only = true, tmpfs = ["/data"] }]
"#;

fn plan<'a>(config: &'a Config, scope: Scope, trusted: bool) -> Plan<'a> {
    Plan {
        build_id: 7,
        attempt: 1,
        repo: "o/web",
        sha: "0123456789abcdef0123456789abcdef01234567",
        reference: "main",
        event: if trusted { "push" } else { "pull_request" },
        version: trusted.then_some("0.0.1-rc.56"),
        run: 56,
        config,
        scope,
        trusted,
        fetch_secret: "kiln-b7-a1-fetch",
        repo_secret: Some("kiln-repo-o-web"),
        cache_claim: Some("kiln-cache-o-web"),
        timeout_secs: 1800,
    }
}

fn containers(pod: &Pod) -> Vec<Container> {
    let spec = pod.spec.clone().unwrap();
    spec.init_containers.unwrap_or_default().into_iter().chain(spec.containers).collect()
}

fn env_secret_names(container: &Container) -> Vec<String> {
    container
        .env
        .clone()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|e| e.value_from?.secret_key_ref.map(|r| r.name))
        .collect()
}

fn mounts(container: &Container) -> Vec<String> {
    container.volume_mounts.clone().unwrap_or_default().into_iter().map(|m| m.name).collect()
}

#[test]
fn credentials_stay_in_the_containers_that_need_them() {
    let config = parse(CONFIG, "web").unwrap();
    let settings = Settings::example();
    let rendered = render(&plan(&config, Scope::Full, true), &settings).unwrap();
    let spec = rendered.pod.spec.clone().unwrap();
    assert_eq!(spec.automount_service_account_token, Some(false));
    assert_eq!(spec.host_users, Some(false));

    for container in containers(&rendered.pod) {
        let name = container.name.as_str();
        let push_credential = env_secret_names(&container).contains(&settings.push_secret);
        assert_eq!(push_credential, name == "push", "push credential in {name}");
        assert_eq!(mounts(&container).contains(&"fetch-token".to_owned()), name == "fetch", "fetch token in {name}");
        assert_eq!(
            mounts(&container).contains(&"build-secrets".to_owned()),
            name.starts_with("image-"),
            "build secrets in {name}"
        );
        let unconfined = container
            .security_context
            .as_ref()
            .and_then(|s| s.seccomp_profile.as_ref())
            .is_some_and(|p| p.type_ == "Unconfined");
        assert_eq!(unconfined, name == "buildkitd", "unconfined seccomp in {name}");
        if container.restart_policy.is_none() && name != "push" && name != "fetch" {
            assert!(
                container.security_context.as_ref().and_then(|s| s.allow_privilege_escalation) == Some(false),
                "{name}"
            );
        }
    }

    let kinds: Vec<TaskKind> = rendered.tasks.iter().map(|t| t.kind).collect();
    assert_eq!(kinds, [TaskKind::Fetch, TaskKind::Test, TaskKind::Image, TaskKind::Push, TaskKind::Verify]);
    let build = containers(&rendered.pod).into_iter().find(|c| c.name == "image-0").unwrap();
    let args = build.args.unwrap().join(" ");
    assert!(args.contains("--opt=build-arg:DEPLOYMENT_ID=0123456789abcdef0123456789abcdef01234567"), "{args}");
    assert!(args.contains("--secret=id=actions_key,src=/kiln/secrets/ACTIONS_KEY"), "{args}");
    assert!(args.contains("type=oci"), "{args}");
}

#[test]
fn pull_requests_get_no_secrets_no_builder_and_their_own_cache() {
    let config = parse(CONFIG, "web").unwrap();
    let settings = Settings::example();
    let rendered = render(&plan(&config, Scope::TestsOnly, false), &settings).unwrap();
    let names: Vec<String> = containers(&rendered.pod).into_iter().map(|c| c.name).collect();
    assert_eq!(names, ["fetch", "test-unit", "done"]);
    for container in containers(&rendered.pod) {
        let secrets = env_secret_names(&container);
        assert!(secrets.is_empty(), "{} reads secrets {secrets:?}", container.name);
    }
    let unit = containers(&rendered.pod).into_iter().find(|c| c.name == "test-unit").unwrap();
    let cache = unit.volume_mounts.unwrap().into_iter().find(|m| m.name == "cache").unwrap();
    assert!(cache.sub_path.unwrap().starts_with("steps-pr/"));
    let volumes: Vec<String> = rendered.pod.spec.unwrap().volumes.unwrap().into_iter().map(|v| v.name).collect();
    assert!(!volumes.contains(&"build-secrets".to_owned()));
}
