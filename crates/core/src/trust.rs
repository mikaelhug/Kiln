#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    DefaultBranchPush,
    Tag { on_default_branch: bool },
    PullRequest,
    Ui { default_branch_head: bool },
    Rerun(Box<Origin>),
}

pub fn grant_secrets(origin: &Origin) -> bool {
    match origin {
        Origin::DefaultBranchPush => true,
        Origin::Tag { on_default_branch } => *on_default_branch,
        Origin::PullRequest => false,
        Origin::Ui { default_branch_head } => *default_branch_head,
        Origin::Rerun(original) => grant_secrets(original),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pull_requests_never_receive_secrets() {
        assert!(!grant_secrets(&Origin::PullRequest));
        assert!(!grant_secrets(&Origin::Rerun(Box::new(Origin::PullRequest))));
        assert!(!grant_secrets(&Origin::Rerun(Box::new(Origin::Rerun(Box::new(Origin::PullRequest))))));
        assert!(!grant_secrets(&Origin::Tag { on_default_branch: false }));
        assert!(!grant_secrets(&Origin::Ui { default_branch_head: false }));
        assert!(grant_secrets(&Origin::DefaultBranchPush));
        assert!(grant_secrets(&Origin::Tag { on_default_branch: true }));
        assert!(grant_secrets(&Origin::Rerun(Box::new(Origin::DefaultBranchPush))));
    }
}
