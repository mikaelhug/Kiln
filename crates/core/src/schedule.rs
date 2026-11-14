#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BuildId(pub i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RepoId(pub i64);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Trigger {
    Tag(String),
    DefaultBranch,
    Manual,
    PullRequest(u64),
}

impl Trigger {
    pub fn priority(&self) -> u8 {
        match self {
            Trigger::Tag(_) => 0,
            Trigger::DefaultBranch | Trigger::Manual => 1,
            Trigger::PullRequest(_) => 2,
        }
    }

    pub fn supersedes(&self, older: &Trigger) -> bool {
        match (self, older) {
            (Trigger::DefaultBranch, Trigger::DefaultBranch) => true,
            (Trigger::PullRequest(new), Trigger::PullRequest(old)) => new == old,
            _ => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Queued {
    pub id: BuildId,
    pub repo: RepoId,
    pub sha: String,
    pub trigger: Trigger,
    pub created_at: i64,
    pub not_before: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Active {
    pub id: BuildId,
    pub repo: RepoId,
}

pub fn next_dispatch(queued: &[Queued], active: &[Active], slots: usize, now: i64) -> Option<BuildId> {
    if active.len() >= slots {
        return None;
    }
    let waits_for_branch_build = |tag: &Queued| {
        matches!(tag.trigger, Trigger::Tag(_))
            && queued.iter().any(|other| {
                other.repo == tag.repo
                    && other.sha == tag.sha
                    && matches!(other.trigger, Trigger::DefaultBranch | Trigger::Manual)
            })
    };
    queued
        .iter()
        .filter(|build| build.not_before <= now && active.iter().all(|running| running.repo != build.repo))
        .filter(|build| !waits_for_branch_build(build))
        .min_by_key(|build| (build.trigger.priority(), build.created_at, build.id))
        .map(|build| build.id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn queued(id: i64, repo: i64, trigger: Trigger, created_at: i64) -> Queued {
        Queued { id: BuildId(id), repo: RepoId(repo), sha: format!("sha{id}"), trigger, created_at, not_before: 0 }
    }

    #[test]
    fn two_slots_shared_by_all_repos_one_build_per_repo() {
        let queue = [
            queued(1, 1, Trigger::DefaultBranch, 10),
            queued(2, 1, Trigger::PullRequest(4), 5),
            queued(3, 2, Trigger::PullRequest(1), 1),
            queued(4, 3, Trigger::Tag("v1.0.0".into()), 20),
        ];
        assert_eq!(next_dispatch(&queue, &[], 2, 100), Some(BuildId(4)));
        let busy = [Active { id: BuildId(4), repo: RepoId(3) }];
        assert_eq!(next_dispatch(&queue, &busy, 2, 100), Some(BuildId(1)));
        let full = [busy[0], Active { id: BuildId(1), repo: RepoId(1) }];
        assert_eq!(next_dispatch(&queue, &full, 2, 100), None);
        let repo_one_busy = [Active { id: BuildId(1), repo: RepoId(1) }];
        assert_eq!(next_dispatch(&queue[..2], &repo_one_busy, 2, 100), None);
    }

    #[test]
    fn a_tag_waits_for_the_branch_build_of_the_same_commit_to_reuse_its_image() {
        let mut branch = queued(1, 1, Trigger::DefaultBranch, 1);
        let mut tag = queued(2, 1, Trigger::Tag("v1.0.0".into()), 2);
        branch.sha = "abc".into();
        tag.sha = "abc".into();
        let queue = [branch, tag];
        assert_eq!(next_dispatch(&queue, &[], 2, 100), Some(BuildId(1)));
        assert_eq!(next_dispatch(&queue[1..], &[], 2, 100), Some(BuildId(2)));
    }

    #[test]
    fn retries_wait_for_their_backoff() {
        let mut retry = queued(1, 1, Trigger::DefaultBranch, 1);
        retry.not_before = 50;
        let queue = [retry];
        assert_eq!(next_dispatch(&queue, &[], 2, 49), None);
        assert_eq!(next_dispatch(&queue, &[], 2, 50), Some(BuildId(1)));
    }

    #[test]
    fn only_newer_heads_of_the_same_line_supersede() {
        assert!(Trigger::DefaultBranch.supersedes(&Trigger::DefaultBranch));
        assert!(Trigger::PullRequest(3).supersedes(&Trigger::PullRequest(3)));
        assert!(!Trigger::PullRequest(3).supersedes(&Trigger::PullRequest(4)));
        assert!(!Trigger::DefaultBranch.supersedes(&Trigger::Tag("v1.0.0".into())));
        assert!(!Trigger::DefaultBranch.supersedes(&Trigger::Manual));
        assert!(!Trigger::Tag("v2.0.0".into()).supersedes(&Trigger::Tag("v1.0.0".into())));
    }
}
