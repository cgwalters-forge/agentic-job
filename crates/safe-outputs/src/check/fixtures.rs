//! The policies the library's tests check against: what agentic-job's
//! `policy` compiles for its own tests' request, which its tests compare
//! with these files, so that the two cannot drift apart.

use crate::contract::Policy;
use crate::link_domains;

/// A run on `bootc-dev/bootc`'s `main` that may open a pull request.
const POLICY: &str = include_str!("../../tests/data/policy.json");

/// A run that pushes to pull request 42's branch, `agent-run-12`.
const PUSH_POLICY: &str = include_str!("../../tests/data/push-policy.json");

fn parsed(json: &str) -> Policy {
    let mut policy: Policy = serde_json::from_str(json).unwrap();
    // Left out of the files, which would otherwise list every top-level
    // domain; `policy` writes them all.
    policy.allowed_domains = link_domains().to_vec();
    policy.validate().unwrap();
    policy
}

pub(crate) fn policy() -> Policy {
    parsed(POLICY)
}

pub(crate) fn push_policy() -> Policy {
    parsed(PUSH_POLICY)
}

/// The head [`push_policy`] pins.
pub(crate) const HEAD: &str = "2222222222222222222222222222222222222222";

/// [`push_policy`] from bounds that name FORK for the branch, as
/// agentic-job's tests compile it.
pub(crate) fn push_policy_to(fork: &str) -> Policy {
    Policy {
        push_repo: Some(fork.to_owned()),
        ..push_policy()
    }
}
