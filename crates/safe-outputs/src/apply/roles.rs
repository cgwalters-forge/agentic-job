//! Caller-only credential routing. A comment must not borrow a credential
//! capable of pushing code when its own secret is missing.

use std::collections::BTreeMap;

use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde_json::Value;

use super::inline::InlineComment;
use super::{
    BranchPush, Close, CommitStatus, LabelChange, Made, NewIssue, Op, Publisher, PullRequest,
};

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

    /// OP's role, by its kind alone, as [`Self::output`] routes requests.
    pub fn op(op: &Op) -> Self {
        match op {
            Op::Inline(_) | Op::Comment { .. } => Self::Review,
            Op::Push(_) => Self::ForkPush,
            Op::Status(_) => Self::Status,
            Op::PullRequest(_) | Op::Issue(_) | Op::Labels(_) | Op::Close(_) => Self::Publication,
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

/// One publisher per role, each with its own credential, for a forge
/// whose publisher holds one token (Forgejo, GitLab): each op goes to its
/// role's ([`Role::op`]). A new pull request is the publication role's,
/// whose publisher git pushes with the fork-push token.
pub struct Routed {
    publishers: BTreeMap<Role, Box<dyn Publisher>>,
    /// Every role's token, longest first: a publisher scrubs its own, and
    /// these go too, should a forge echo another role's.
    tokens: Vec<String>,
}

impl Routed {
    /// MAKE builds each role's publisher from its credential, and, for
    /// the publication role, the fork-push token its git push uses. Before
    /// any write, every credential must be present and authenticate as its
    /// bound numeric actor; an `app_id` refuses, as only GitHub has Apps.
    pub fn new(
        credentials: &BTreeMap<Role, Credential>,
        make: impl Fn(Role, &Credential, Option<&str>) -> Result<Box<dyn Publisher>>,
    ) -> Result<Self> {
        let bound = |role: Role| {
            credentials
                .get(&role)
                .context("role mapping must bind all four roles")
        };
        for role in Role::ALL {
            let credential = bound(role)?;
            ensure!(
                credential.identity.app_id.is_none(),
                "role {role:?} has an app_id, which only GitHub has; bind actor_id alone"
            );
            ensure!(
                !credential.token.is_empty(),
                "{} is missing for configured role",
                role.token_env()
            );
        }
        let mut routed = Self {
            publishers: BTreeMap::new(),
            tokens: credentials.values().map(|c| c.token.clone()).collect(),
        };
        routed
            .tokens
            .sort_by_key(|token| std::cmp::Reverse(token.len()));
        let push = bound(Role::ForkPush)?.token.as_str();
        for role in Role::ALL {
            let credential = bound(role)?;
            let publisher = make(
                role,
                credential,
                (role == Role::Publication).then_some(push),
            )?;
            let actor =
                routed.shown(publisher.actor_id().with_context(|| {
                    format!("reading {}'s authenticated user", role.token_env())
                }))?;
            ensure!(
                actor == credential.identity.actor_id,
                "{} authenticates as actor {actor}, not the bound {}",
                role.token_env(),
                credential.identity.actor_id
            );
            routed.publishers.insert(role, publisher);
        }
        Ok(routed)
    }

    fn at(&self, role: Role) -> &dyn Publisher {
        // `new` bound every role.
        self.publishers[&role].as_ref()
    }

    /// RESULT, its error without any role's token.
    fn shown<T>(&self, result: Result<T>) -> Result<T> {
        result.map_err(|err| {
            let text = format!("{err:#}");
            anyhow::anyhow!(
                self.tokens
                    .iter()
                    .fold(text, |text, token| text.replace(token, "[token]"))
            )
        })
    }
}

impl Publisher for Routed {
    fn inline_comment(&self, comment: &InlineComment) -> Result<String> {
        self.shown(self.at(Role::Review).inline_comment(comment))
    }

    fn preflight(&self, op: &Op) -> Result<()> {
        self.shown(self.at(Role::op(op)).preflight(op))
    }

    /// Each role's publisher reads its own ops in their order, so that a
    /// projection across ops (Forgejo's labels) sees all of them.
    fn preflight_batch(&self, ops: &[Op]) -> Result<()> {
        for role in Role::ALL {
            let mine: Vec<Op> = ops
                .iter()
                .filter(|op| Role::op(op) == role)
                .cloned()
                .collect();
            if !mine.is_empty() {
                self.shown(self.at(role).preflight_batch(&mine))?;
            }
        }
        Ok(())
    }

    fn comment(&self, issue: u64, body: &str) -> Result<Made> {
        self.shown(self.at(Role::Review).comment(issue, body))
    }

    fn open_pull_request(&self, pr: &PullRequest) -> Result<Made> {
        self.shown(self.at(Role::Publication).open_pull_request(pr))
    }

    fn push_to_branch(&self, push: &BranchPush) -> Result<String> {
        self.shown(self.at(Role::ForkPush).push_to_branch(push))
    }

    fn create_issue(&self, issue: &NewIssue) -> Result<String> {
        self.shown(self.at(Role::Publication).create_issue(issue))
    }

    fn change_labels(&self, change: &LabelChange) -> Result<String> {
        self.shown(self.at(Role::Publication).change_labels(change))
    }

    fn close(&self, close: &Close) -> Result<String> {
        self.shown(self.at(Role::Publication).close(close))
    }

    fn status(&self, status: &CommitStatus) -> Result<String> {
        self.shown(self.at(Role::Status).status(status))
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::super::testing::{pull_request_branch, repository};
    use super::super::{Kind, State, publish};
    use super::*;
    use serde_json::json;

    /// What a [`Fake`] was asked: its role, the method, and the token and
    /// push token it was made with.
    type Log = Rc<RefCell<Vec<String>>>;

    /// A publisher that answers each write with its role's name, or, if
    /// FAIL, a refusal echoing every role's token, as a forge might.
    struct Fake {
        role: Role,
        actor: Result<u64, ()>,
        log: Log,
        echo: String,
        fail: bool,
    }

    impl Fake {
        fn wrote(&self, what: &str) -> Result<String> {
            self.log
                .borrow_mut()
                .push(format!("{:?} {what}", self.role));
            ensure!(!self.fail, "{what}: HTTP 403: {}", self.echo);
            Ok(format!("{:?}", self.role))
        }

        fn made(&self, what: &str) -> Result<Made> {
            Ok(Made {
                url: self.wrote(what)?,
                number: None,
                id: None,
                node_id: None,
            })
        }
    }

    impl Publisher for Fake {
        fn preflight(&self, _: &Op) -> Result<()> {
            unreachable!("Routed reads a batch")
        }

        fn preflight_batch(&self, ops: &[Op]) -> Result<()> {
            assert!(ops.iter().all(|op| Role::op(op) == self.role));
            self.log
                .borrow_mut()
                .push(format!("{:?} preflight {}", self.role, ops.len()));
            Ok(())
        }

        fn comment(&self, _: u64, _: &str) -> Result<Made> {
            self.made("comment")
        }

        fn open_pull_request(&self, _: &PullRequest) -> Result<Made> {
            self.made("open")
        }

        fn push_to_branch(&self, _: &BranchPush) -> Result<String> {
            self.wrote("push")
        }

        fn create_issue(&self, _: &NewIssue) -> Result<String> {
            self.wrote("issue")
        }

        fn change_labels(&self, _: &LabelChange) -> Result<String> {
            self.wrote("labels")
        }

        fn close(&self, _: &Close) -> Result<String> {
            self.wrote("close")
        }

        fn status(&self, _: &CommitStatus) -> Result<String> {
            self.wrote("status")
        }

        fn actor_id(&self) -> Result<u64> {
            self.log.borrow_mut().push(format!("{:?} user", self.role));
            self.actor
                .map_err(|()| anyhow::anyhow!("GET /user: HTTP 403: {}", self.echo))
        }
    }

    /// Each role bound to its own actor, 100 and up, with its own token,
    /// assembled here rather than written out.
    fn credentials() -> BTreeMap<Role, Credential> {
        Role::ALL
            .into_iter()
            .zip(100..)
            .map(|(role, actor_id)| {
                let identity = Identity {
                    actor_id,
                    app_id: None,
                };
                let token = ["fake", role.token_env(), "credential"].join("-");
                (role, Credential { identity, token })
            })
            .collect()
    }

    /// CREDENTIALS routed to fakes, each authenticating as ACTOR says,
    /// whose writes FAIL if asked.
    fn routed(
        credentials: &BTreeMap<Role, Credential>,
        actor: impl Fn(Role, &Credential) -> Result<u64, ()>,
        fail: bool,
    ) -> (Result<Routed>, Log) {
        let log = Log::default();
        let echo: Vec<&str> = credentials.values().map(|c| c.token.as_str()).collect();
        let routed = Routed::new(credentials, |role, credential, push| {
            log.borrow_mut().push(format!(
                "{role:?} made with {} pushing with {push:?}",
                credential.token
            ));
            Ok(Box::new(Fake {
                role,
                actor: actor(role, credential),
                log: log.clone(),
                echo: echo.join(" "),
                fail,
            }))
        });
        (routed, log)
    }

    /// One op of each kind, and its fixed role.
    fn ops() -> (Vec<Op>, Vec<Role>, [tempfile::TempDir; 2]) {
        let (root, pr) = repository();
        let (branch, push) = pull_request_branch();
        let status = CommitStatus {
            sha: "4".repeat(40),
            state: State::Success,
            target_url: "https://forge.example/run".into(),
            context: "example-ci".into(),
        };
        let (ops, roles) = [
            (Op::Status(status), Role::Status),
            (
                Op::Comment {
                    issue: 5,
                    body: "Done.".into(),
                },
                Role::Review,
            ),
            (Op::Push(push), Role::ForkPush),
            (Op::PullRequest(pr), Role::Publication),
            (
                Op::Issue(NewIssue {
                    title: "t".into(),
                    body: "b".into(),
                    labels: vec![],
                }),
                Role::Publication,
            ),
            (
                Op::Labels(LabelChange {
                    item: 5,
                    kind: None,
                    add: true,
                    labels: vec!["bug".into()],
                }),
                Role::Publication,
            ),
            (
                Op::Close(Close {
                    number: 5,
                    kind: Kind::Issue,
                    body: "b".into(),
                }),
                Role::Publication,
            ),
        ]
        .into_iter()
        .unzip();
        (ops, roles, [root, branch])
    }

    /// Every op goes to its fixed role's publisher, by its kind alone; a
    /// new pull request to publication's, which pushes with fork-push's
    /// token.
    #[test]
    fn routes_each_op_by_its_kind() {
        let (ops, roles, _dirs) = ops();
        let credentials = credentials();
        let (routed, log) = routed(&credentials, |_, c| Ok(c.identity.actor_id), false);
        let routed = routed.unwrap();
        let made = publish(&ops, &routed).unwrap();
        let roles: Vec<String> = roles.iter().map(|role| format!("{role:?}")).collect();
        assert_eq!(made, roles);
        let log = log.borrow();
        let push = &credentials[&Role::ForkPush].token;
        for role in Role::ALL {
            let token = &credentials[&role].token;
            let expected = match role {
                Role::Publication => {
                    format!("Publication made with {token} pushing with Some({push:?})")
                }
                _ => format!("{role:?} made with {token} pushing with None"),
            };
            assert!(log.contains(&expected), "{log:?}");
        }
        // Every identity is read before the first preflight or write.
        let first = log
            .iter()
            .position(|line| !line.contains(" made with ") && !line.ends_with(" user"));
        assert!(
            log[..first.unwrap()]
                .iter()
                .filter(|line| line.ends_with(" user"))
                .count()
                == 4,
            "{log:?}"
        );
        assert!(
            log.contains(&"Publication preflight 4".to_owned()),
            "{log:?}"
        );
    }

    /// No role's token is in an error, whichever role's publisher made it.
    #[test]
    fn every_role_token_is_scrubbed_from_errors() {
        let (ops, roles, _dirs) = ops();
        let credentials = credentials();
        let (routed, _log) = routed(&credentials, |_, c| Ok(c.identity.actor_id), true);
        let routed = routed.unwrap();
        for (op, role) in ops.iter().zip(roles) {
            let err = format!(
                "{:#}",
                publish(std::slice::from_ref(op), &routed).unwrap_err()
            );
            assert!(err.contains("HTTP 403: [token] [token]"), "{role:?}: {err}");
            for credential in credentials.values() {
                assert!(!err.contains(&credential.token), "{role:?}: {err}");
            }
        }
    }

    /// A missing, unauthenticated or wrong credential, or an `app_id`,
    /// refuses before any write, and never falls back to another.
    #[test]
    fn bound_identity_is_checked_before_any_write() {
        type Case = (
            &'static str,
            fn(&mut BTreeMap<Role, Credential>),
            fn(Role, &Credential) -> Result<u64, ()>,
            Option<&'static str>,
        );
        let right: fn(Role, &Credential) -> Result<u64, ()> = |_, c| Ok(c.identity.actor_id);
        let cases: [Case; 6] = [
            ("bound", |_| {}, right, None),
            (
                "an empty token",
                |c| c.get_mut(&Role::Review).unwrap().token.clear(),
                right,
                Some("REVIEW_TOKEN is missing for configured role"),
            ),
            (
                "an unbound role",
                |c| {
                    c.remove(&Role::Status);
                },
                right,
                Some("role mapping must bind all four roles"),
            ),
            (
                "an app_id",
                |c| c.get_mut(&Role::Status).unwrap().identity.app_id = Some(9),
                right,
                Some("role Status has an app_id, which only GitHub has"),
            ),
            (
                "another actor",
                |_| {},
                |role, c| {
                    Ok(if role == Role::ForkPush {
                        999
                    } else {
                        c.identity.actor_id
                    })
                },
                Some("PUSH_TOKEN authenticates as actor 999, not the bound 102"),
            ),
            (
                "an unreadable user",
                |_| {},
                |role, c| {
                    if role == Role::Publication {
                        Err(())
                    } else {
                        Ok(c.identity.actor_id)
                    }
                },
                Some("reading PUBLICATION_TOKEN's authenticated user: GET /user: HTTP 403"),
            ),
        ];
        for (what, change, actor, refusal) in cases {
            let mut credentials = credentials();
            change(&mut credentials);
            let (routed, log) = routed(&credentials, actor, false);
            match (routed, refusal) {
                (Ok(_), None) => {}
                (Err(err), Some(refusal)) => {
                    let err = format!("{err:#}");
                    assert!(err.contains(refusal), "{what}: {err}");
                    for credential in credentials.values().filter(|c| !c.token.is_empty()) {
                        assert!(!err.contains(&credential.token), "{what}: {err}");
                    }
                }
                (Ok(_), Some(refusal)) => panic!("{what}: accepted, not {refusal}"),
                (Err(err), None) => panic!("{what}: {err:#}"),
            }
            let log = log.borrow();
            // Nothing but construction and identity reads happened.
            assert!(
                log.iter()
                    .all(|line| line.contains(" made with ") || line.ends_with(" user")),
                "{what}: {log:?}"
            );
            if matches!(what, "an empty token" | "an unbound role" | "an app_id") {
                assert!(log.is_empty(), "{what}: {log:?}");
            }
        }
    }

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
