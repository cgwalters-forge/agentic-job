//! Caller-only credential routing. A comment must not borrow a credential
//! capable of pushing code when its own secret is missing.

use std::collections::BTreeMap;

use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde_json::Value;

pub const BINDINGS_ENV: &str = "APPLY_ROLE_BINDINGS";

/// Fixed operations, never read from a request or an agent's output.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Status,
    Review,
    ForkPush,
    Publication,
}

impl Role {
    pub const ALL: [Self; 4] = [
        Self::Status,
        Self::Review,
        Self::ForkPush,
        Self::Publication,
    ];

    pub fn token_env(self) -> &'static str {
        match self {
            Self::Status => "STATUS_TOKEN",
            Self::Review => "REVIEW_TOKEN",
            Self::ForkPush => "PUSH_TOKEN",
            Self::Publication => "PUBLICATION_TOKEN",
        }
    }

    pub fn output(output: &str) -> Self {
        match output {
            "add_comment" | "create_pull_request_review_comment" => Self::Review,
            "push_to_pull_request_branch" => Self::ForkPush,
            _ => Self::Publication,
        }
    }
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    /// Numeric authenticated actor, not a mutable login or an app slug.
    pub actor_id: u64,
    /// Trusted minting caller's App ID; used for REST object provenance.
    pub app_id: Option<u64>,
}

impl Identity {
    pub fn owns(&self, object: &Value) -> bool {
        object["user"]["id"].as_u64() == Some(self.actor_id)
            && match self.app_id {
                Some(id) => object["performed_via_github_app"]["id"].as_u64() == Some(id),
                None => object["performed_via_github_app"].is_null(),
            }
    }
}

#[derive(Clone)]
pub struct Credential {
    pub identity: Identity,
    pub token: String,
}

pub fn read(get: &dyn Fn(&str) -> Option<String>) -> Result<Option<BTreeMap<Role, Credential>>> {
    let Some(text) = get(BINDINGS_ENV).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    ensure!(text.len() <= 4096, "role bindings exceed 4096 bytes");
    let identities: BTreeMap<Role, Identity> =
        serde_json::from_str(&text).context("invalid caller role bindings")?;
    let mut credentials = BTreeMap::new();
    for role in Role::ALL {
        let identity = identities
            .get(&role)
            .context("role mapping must bind all four roles")?;
        ensure!(
            identity.actor_id > 0 && identity.app_id != Some(0),
            "role identity IDs must be positive"
        );
        credentials.insert(
            role,
            Credential {
                identity: identity.clone(),
                token: get(role.token_env()).unwrap_or_default(),
            },
        );
    }
    Ok(Some(credentials))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn fixed_operation_table() {
        for (operation, role) in [
            ("add_comment", Role::Review),
            ("create_pull_request_review_comment", Role::Review),
            ("push_to_pull_request_branch", Role::ForkPush),
            ("create_pull_request", Role::Publication),
            ("create_issue", Role::Publication),
            ("close_issue", Role::Publication),
        ] {
            assert_eq!(Role::output(operation), role);
        }
        assert_eq!(Role::Status.token_env(), "STATUS_TOKEN");
    }

    #[test]
    fn caller_mapping_is_optional_but_never_partial() {
        let mapping = json!({
            "status": {"actor_id": 1, "app_id": 11},
            "review": {"actor_id": 2},
            "fork_push": {"actor_id": 3},
            "publication": {"actor_id": 4}
        });
        assert!(read(&|_| None).unwrap().is_none());
        for (text, accepted) in [
            (mapping.to_string(), true),
            ("{}".to_owned(), false),
            ("{\"status\":{\"actor_id\":1}}".to_owned(), false),
            (mapping.to_string().replace("actor_id", "login"), false),
            (
                mapping
                    .to_string()
                    .replace("\"actor_id\":1", "\"actor_id\":0"),
                false,
            ),
            (" ".repeat(4097), false),
        ] {
            let result = read(&|key| (key == BINDINGS_ENV).then(|| text.clone()));
            assert_eq!(result.is_ok(), accepted, "{text}");
        }
    }

    #[test]
    fn app_provenance_is_not_login_text() {
        let identity = Identity {
            actor_id: 7,
            app_id: Some(42),
        };
        for (actor, app, accepted) in [
            (7, Some(42), true),
            (8, Some(42), false),
            (7, Some(43), false),
            (7, None, false),
        ] {
            let object = json!({"user": {"id": actor, "login": "trusted[bot]"},
                "performed_via_github_app": app.map(|id| json!({"id": id}))});
            assert_eq!(identity.owns(&object), accepted);
        }
    }
}
