use kiln_store::{
    BuildFilter, BuildState, BuildUpdate, GitHubApp, NewBuild, Person, RepoSync, Retention, Store, Trigger,
};

fn repo(store: &Store, id: i64, name: &str) {
    store
        .upsert_repo(&RepoSync {
            id,
            installation_id: 1,
            owner: "o".into(),
            name: name.into(),
            default_branch: "main".into(),
            private: true,
            archived: false,
            seen_at: 0,
        })
        .unwrap();
}

fn build(repo_id: i64, trigger: Trigger, sha: &str, pr: Option<i64>, at: i64) -> NewBuild {
    NewBuild {
        repo_id,
        trigger,
        ref_name: "main".into(),
        pr_number: pr,
        sha: sha.into(),
        message: Some("Fix things\n\nbody".into()),
        dedupe_key: format!("{}:{}:{sha}", trigger.as_str(), pr.unwrap_or(0)),
        actor: None,
        trusted: pr.is_none(),
        config: None,
        state: BuildState::Queued,
        failure: None,
        detail: None,
        rerun_of: None,
        created_at: at,
    }
}

#[test]
fn builds_are_deduplicated_numbered_per_repo_and_found_for_supersede() {
    let store = Store::in_memory().unwrap();
    repo(&store, 1, "web");
    repo(&store, 2, "api");
    let first = store.insert_build(&build(1, Trigger::Push, "aaa", None, 1)).unwrap().unwrap();
    assert_eq!(store.insert_build(&build(1, Trigger::Push, "aaa", None, 2)).unwrap(), None);
    let pr = store.insert_build(&build(1, Trigger::PullRequest, "ppp", Some(4), 3)).unwrap().unwrap();
    let second = store.insert_build(&build(1, Trigger::Push, "bbb", None, 4)).unwrap().unwrap();
    let other = store.insert_build(&build(2, Trigger::Push, "ccc", None, 5)).unwrap().unwrap();

    assert_eq!(store.build(second).unwrap().unwrap().number, 3);
    assert_eq!(store.build(other).unwrap().unwrap().number, 1);
    assert_eq!(store.build(first).unwrap().unwrap().title(), "Fix things");

    let newest = store.build(second).unwrap().unwrap();
    let older: Vec<i64> = store.same_line(&newest).unwrap().iter().map(|b| b.id).collect();
    assert_eq!(older, vec![first]);
    let pr_build = store.build(pr).unwrap().unwrap();
    assert!(store.same_line(&pr_build).unwrap().is_empty());
}

#[test]
fn tag_releases_reuse_only_fully_built_successful_candidates() {
    let store = Store::in_memory().unwrap();
    repo(&store, 1, "web");
    let id = store.insert_build(&build(1, Trigger::Push, "aaa", None, 1)).unwrap().unwrap();
    store.set_images(id, &["migrate".into(), "web".into()]).unwrap();
    store.set_candidate(id, "migrate", "sha256:1").unwrap();
    store.update_build(id, &BuildUpdate { state: Some(BuildState::Succeeded), ..Default::default() }).unwrap();
    assert!(store.verified_candidate(1, "aaa").unwrap().is_none(), "one image has no digest yet");
    store.set_candidate(id, "web", "sha256:2").unwrap();
    let (found, images) = store.verified_candidate(1, "aaa").unwrap().unwrap();
    assert_eq!(found, id);
    assert_eq!(images.iter().map(|i| i.name.as_str()).collect::<Vec<_>>(), ["migrate", "web"]);
}

#[test]
fn retention_never_removes_active_builds() {
    let store = Store::in_memory().unwrap();
    repo(&store, 1, "web");
    let old = store.insert_build(&build(1, Trigger::Push, "aaa", None, 0)).unwrap().unwrap();
    let running = store.insert_build(&build(1, Trigger::Push, "bbb", None, 0)).unwrap().unwrap();
    store.update_build(old, &BuildUpdate { state: Some(BuildState::Failed), ..Default::default() }).unwrap();
    store.update_build(running, &BuildUpdate { state: Some(BuildState::Running), ..Default::default() }).unwrap();
    let removed = store.prune(400 * 86_400, &Retention::default()).unwrap();
    assert_eq!(removed, vec![old]);
    let left = store.builds(&BuildFilter { limit: 10, ..Default::default() }).unwrap();
    assert_eq!(left.iter().map(|b| b.id).collect::<Vec<_>>(), vec![running]);
}

#[test]
fn retention_keeps_each_repositorys_latest_and_latest_branch_build() {
    let store = Store::in_memory().unwrap();
    repo(&store, 1, "web");
    let branch = store.insert_build(&build(1, Trigger::Push, "aaa", None, 0)).unwrap().unwrap();
    let pull = store.insert_build(&build(1, Trigger::PullRequest, "bbb", Some(3), 0)).unwrap().unwrap();
    for id in [branch, pull] {
        store.update_build(id, &BuildUpdate { state: Some(BuildState::Succeeded), ..Default::default() }).unwrap();
    }
    assert!(store.prune(400 * 86_400, &Retention::default()).unwrap().is_empty());
    let next = store.insert_build(&build(1, Trigger::Push, "ccc", None, 0)).unwrap().unwrap();
    assert_eq!(store.build(next).unwrap().unwrap().number, 3, "numbering continues after pruning");
}

fn app(owner_is_user: bool) -> GitHubApp {
    GitHubApp {
        app_id: 1,
        slug: "kiln".into(),
        name: "Kiln".into(),
        html_url: "https://github.com/apps/kiln".into(),
        client_id: "Iv1".into(),
        client_secret: "secret".into(),
        webhook_secret: "hook".into(),
        private_key: "pem".into(),
        owner_login: "owner".into(),
        owner_id: 10,
        owner_is_user,
        claim_token: Some("claim".into()),
        registry_username: None,
        registry_token: None,
        connected_at: 0,
        registry_expires: None,
    }
}

fn person(id: i64) -> Person {
    Person { user_id: id, login: format!("user{id}"), added_by: "test".into(), added_at: 0 }
}

#[test]
fn only_the_connecting_browser_claims_an_organization_install_and_only_once() {
    let store = Store::in_memory().unwrap();
    assert!(!store.claim_first_person("claim", &person(20)).unwrap(), "nothing is connected yet");
    assert!(store.connect_github(&app(false)).unwrap());
    assert!(!store.connect_github(&app(false)).unwrap(), "a connection is never replaced");
    assert!(!store.claim_first_person("guess", &person(20)).unwrap());
    assert!(store.claim_first_person("claim", &person(20)).unwrap());
    assert!(!store.claim_first_person("claim", &person(30)).unwrap(), "the claim is single use");
    assert!(store.is_person(20).unwrap() && !store.is_person(30).unwrap());
    assert!(!store.remove_person(20).unwrap(), "the last person stays");
}

#[test]
fn a_personal_install_lets_its_owner_in() {
    let store = Store::in_memory().unwrap();
    store.connect_github(&app(true)).unwrap();
    assert!(store.is_person(10).unwrap());
    assert!(!store.claim_first_person("claim", &person(20)).unwrap(), "the owner is already in");
}
