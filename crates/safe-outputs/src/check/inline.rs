//! The one item and head a neutral inline comment is held to: trusted
//! check context, which check writes into its report and apply reads
//! back. Here, so that check and apply share the type without either
//! depending on the other; how apply publishes the comment stays apply's.

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use crate::contract::{COMMIT_RE, Policy};

/// Trusted check context, not a field in an agent's request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckedTarget {
    pub repo: String,
    pub number: u64,
    pub head: String,
}

/// The target of neutral inline comments, from the caller's fixed
/// COMMENT target and the admitted HEAD: the policy's repository and a
/// full commit id, or a refusal.
pub fn checked_target(
    comment: Option<(&str, u64)>,
    head: Option<&str>,
    policy: &Policy,
) -> Result<CheckedTarget> {
    let (repo, number) = comment.context("neutral inline comments need a fixed comment target")?;
    ensure!(
        repo.eq_ignore_ascii_case(&policy.repo) && number > 0,
        "neutral inline comments need the policy's repository and fixed PR/MR"
    );
    let head = head
        .filter(|head| COMMIT_RE.is_match(head))
        .context("neutral inline comments require --review-head, the admitted full head SHA")?;
    Ok(CheckedTarget {
        repo: policy.repo.clone(),
        number,
        head: head.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::fixtures::policy;

    const HEAD: &str = "abcdef0123456789abcdef0123456789abcdef01";

    #[test]
    fn only_the_policy_repository_and_a_full_head() {
        let policy = policy();
        let target = checked_target(Some(("Bootc-Dev/Bootc", 7)), Some(HEAD), &policy).unwrap();
        assert_eq!(
            target,
            CheckedTarget {
                repo: "bootc-dev/bootc".into(),
                number: 7,
                head: HEAD.into(),
            }
        );
        for (comment, head) in [
            (None, Some(HEAD)),
            (Some(("other/repo", 7)), Some(HEAD)),
            (Some(("bootc-dev/bootc", 0)), Some(HEAD)),
            (Some(("bootc-dev/bootc", 7)), None),
            (Some(("bootc-dev/bootc", 7)), Some("abcdef01")),
            (Some(("bootc-dev/bootc", 7)), Some(&HEAD.to_uppercase())),
        ] {
            assert!(
                checked_target(comment, head, &policy).is_err(),
                "{comment:?} {head:?}"
            );
        }
    }
}
