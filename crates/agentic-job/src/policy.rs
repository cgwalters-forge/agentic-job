//! `agentic-job policy`: check one run's request against the caller's
//! bounds and print `policy.json`. Step 4 of docs/plan.md.
//!
//! The request (which repository, which base, which outputs and how many)
//! comes from whoever started the run. The bounds are a file the caller
//! keeps in its own repository. A request outside them is refused here,
//! on a machine the agent never touches and before the agent's machine
//! starts, and what is within them becomes the run's [`Policy`]: the one
//! thing `run`, gh-aw's collector and `check` all read.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use anyhow::{Context, Result, ensure};
use clap::ValueEnum;
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::exit::Exit;

/// The output type that carries a patch.
pub const CREATE_PULL_REQUEST: &str = "create_pull_request";

/// The output types a bounds file may list. Each is one that gh-aw's
/// collector validates with the configuration written here; another type
/// needs settings of its own, so it is added here with them and not by a
/// caller's file alone.
pub const OUTPUT_TYPES: &[&str] = &[
    CREATE_PULL_REQUEST,
    "add_comment",
    "noop",
    "missing_tool",
    "missing_data",
];

/// A run hands back one patch, so at most one pull request.
const MAX_PULL_REQUESTS: u32 = 1;

/// `--outputs all`: every type the bounds list.
const ALL_OUTPUTS: &str = "all";

/// `--max-outputs max`: as many as the bounds allow.
const MAX_OUTPUTS: &str = "max";

/// gh-aw states a patch's size cap in units of this many bytes.
const KB: u64 = 1024;

/// The files a patch may not touch, at any depth: the defaults gh-aw's
/// compiler writes for `create-pull-request` (v0.90.1). A bounds file can
/// take names off this list for the repositories it names, and cannot add
/// to it.
pub const DEFAULT_PROTECTED_FILES: &[&str] = &[
    "package.json",
    "bun.lockb",
    "bunfig.toml",
    "deno.json",
    "deno.jsonc",
    "deno.lock",
    "global.json",
    "NuGet.Config",
    "Directory.Packages.props",
    "mix.exs",
    "mix.lock",
    "go.mod",
    "go.sum",
    "stack.yaml",
    "stack.yaml.lock",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "settings.gradle",
    "settings.gradle.kts",
    "gradle.properties",
    "package-lock.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "npm-shrinkwrap.json",
    "requirements.txt",
    "Pipfile",
    "Pipfile.lock",
    "pyproject.toml",
    "setup.py",
    "setup.cfg",
    "Gemfile",
    "Gemfile.lock",
    "uv.lock",
    "CODEOWNERS",
    "DESIGN.md",
    "README.md",
    "CONTRIBUTING.md",
    "SECURITY.md",
    "CODE_OF_CONDUCT.md",
    "CLAUDE.md",
    "AGENTS.md",
];

static REPO_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$").expect("a valid pattern"));

static BASE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z0-9_][A-Za-z0-9_./-]{0,199}$").expect("a valid pattern"));

/// The request's values may start with a dash: they are a stranger's text,
/// and one that does is refused with a reason like any other.
#[derive(Debug, clap::Args)]
pub struct Args {
    /// The caller's bounds file
    #[arg(long, value_name = "FILE")]
    pub allow: PathBuf,
    /// The repository the agent works on, as OWNER/NAME
    #[arg(long, value_name = "REPO", allow_hyphen_values = true)]
    pub repo: String,
    /// Where `run` clones that repository from: https://HOST/OWNER/NAME
    #[arg(long, value_name = "URL", allow_hyphen_values = true)]
    pub clone_url: String,
    /// The branch the agent starts from
    #[arg(long, value_name = "BRANCH", allow_hyphen_values = true)]
    pub base: String,
    /// What the run may hand back: a branch, or an analysis only
    #[arg(long, value_enum)]
    pub kind: Kind,
    /// The output types requested, comma-separated, or "all"
    #[arg(long, value_name = "LIST", allow_hyphen_values = true)]
    pub outputs: String,
    /// The most outputs in all and of any one type, or "max"
    #[arg(long, value_name = "N", allow_hyphen_values = true)]
    pub max_outputs: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Branch,
    Analysis,
}

/// The caller's bounds file (`--allow`), in TOML.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bounds {
    /// The hosts a clone URL may name, exactly (`github.com`).
    pub hosts: Vec<String>,
    /// Globs of OWNER/NAME; see [`glob`].
    pub repos: Vec<String>,
    /// Globs of base branches.
    pub bases: Vec<String>,
    /// The most outputs a run may hand back in all.
    pub max_outputs: u32,
    /// The largest patch, in bytes.
    pub max_patch_bytes: u64,
    /// The most files a patch may touch.
    pub max_patch_files: u32,
    /// The output types a run may ask for, and the ceiling of each.
    pub outputs: BTreeMap<String, OutputLimit>,
    /// Files exempt from protection.
    #[serde(default)]
    pub unprotected_files: Unprotected,
}

/// Where names come off [`DEFAULT_PROTECTED_FILES`]: only in the
/// repositories named exactly (no globs), only the names listed. So the
/// rest of the list, the top-level dot-folders and everything `check`
/// refuses on its own stay protected everywhere.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Unprotected {
    #[serde(default)]
    pub repos: Vec<String>,
    #[serde(default)]
    pub files: Vec<String>,
}

/// How many outputs of one type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputLimit {
    pub max: u32,
}

/// One run's request, as the command line gives it.
#[derive(Debug, Clone, Copy)]
pub struct Request<'a> {
    pub repo: &'a str,
    pub clone_url: &'a str,
    pub base: &'a str,
    pub kind: Kind,
    pub outputs: &'a str,
    pub max_outputs: &'a str,
}

/// What `policy.json` holds: the request, once it is within the bounds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub repo: String,
    /// What `run` clones.
    pub clone_url: String,
    pub base: String,
    pub kind: Kind,
    /// The most outputs in all.
    pub max_outputs: u32,
    pub max_patch_bytes: u64,
    /// The configuration gh-aw's collector reads (its `config.json`), and
    /// the rules `check` applies to a patch.
    pub safe_outputs: SafeOutputs,
}

/// gh-aw's safe-outputs configuration: a table per output type allowed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SafeOutputs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub create_pull_request: Option<PullRequest>,
    #[serde(flatten)]
    pub others: BTreeMap<String, OutputLimit>,
}

/// `create_pull_request`'s configuration, in gh-aw's names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PullRequest {
    pub max: u32,
    /// File names a patch may not touch, at any depth.
    pub protected_files: Vec<String>,
    /// Whether everything under a top-level directory whose name starts
    /// with a dot (`.github/`) is protected as well.
    pub protect_top_level_dot_folders: bool,
    pub protected_files_policy: ProtectedFilesPolicy,
    pub draft: bool,
    /// The largest patch, in KB.
    pub max_patch_size: u64,
    pub max_patch_files: u32,
}

/// What gh-aw does with a patch that touches a protected file. Only
/// refusing it can be written here, so a policy that says anything else
/// does not parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProtectedFilesPolicy {
    Blocked,
}

impl SafeOutputs {
    /// The ceiling of an output type, if the run may hand it back.
    pub fn max_of(&self, output: &str) -> Option<u32> {
        if output == CREATE_PULL_REQUEST {
            self.create_pull_request.as_ref().map(|pr| pr.max)
        } else {
            self.others.get(output).map(|limit| limit.max)
        }
    }

    /// The output types the run may hand back.
    pub fn types(&self) -> impl Iterator<Item = &str> {
        self.create_pull_request
            .iter()
            .map(|_| CREATE_PULL_REQUEST)
            .chain(self.others.keys().map(String::as_str))
    }
}

impl Policy {
    /// Reads the `policy.json` that `policy` printed.
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading the policy {}", path.display()))?;
        let policy: Self = serde_json::from_str(&text)
            .with_context(|| format!("{} is not a policy.json", path.display()))?;
        policy
            .validate()
            .with_context(|| format!("{} is not a policy `policy` wrote", path.display()))?;
        Ok(policy)
    }

    /// What the types cannot say: `policy` writes only the output types
    /// that have a configuration here, and only draft pull requests with
    /// the dot-folders protected.
    fn validate(&self) -> Result<()> {
        for output in self.safe_outputs.others.keys() {
            ensure!(
                OUTPUT_TYPES.contains(&output.as_str()) && output != CREATE_PULL_REQUEST,
                "the output type {output:?} has no configuration here"
            );
        }
        if let Some(pr) = &self.safe_outputs.create_pull_request {
            ensure!(pr.draft, "a pull request that is not a draft");
            ensure!(
                pr.protect_top_level_dot_folders,
                "top-level dot-folders are not protected"
            );
            ensure!(pr.max == MAX_PULL_REQUESTS, "more than one pull request");
        }
        Ok(())
    }
}

/// A glob as a pattern: `*` is anything within one path segment, `**`
/// anything at all, and every other character itself. Case is ignored, as
/// forges ignore it in names. This is gh-aw's glob syntax
/// (`glob_pattern_helpers.cjs`).
fn glob(pattern: &str) -> Result<Regex, regex::Error> {
    let body = pattern
        .split("**")
        .map(|part| {
            part.split('*')
                .map(regex::escape)
                .collect::<Vec<_>>()
                .join("[^/]*")
        })
        .collect::<Vec<_>>()
        .join(".*");
    Regex::new(&format!("(?i)^{body}$"))
}

fn glob_matches(patterns: &[String], value: &str) -> bool {
    patterns
        .iter()
        .any(|pattern| glob(pattern).is_ok_and(|re| re.is_match(value)))
}

impl Bounds {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading the bounds file {}", path.display()))?;
        let bounds: Self = toml::from_str(&text)
            .with_context(|| format!("parsing the bounds file {}", path.display()))?;
        bounds
            .validate()
            .with_context(|| format!("in the bounds file {}", path.display()))?;
        Ok(bounds)
    }

    /// A bounds file that bounds nothing, or asks for what cannot be, is
    /// the caller's mistake and not a request to refuse.
    fn validate(&self) -> Result<()> {
        for (name, list) in [
            ("hosts", &self.hosts),
            ("repos", &self.repos),
            ("bases", &self.bases),
        ] {
            ensure!(!list.is_empty(), "`{name}` is empty, so no run is allowed");
        }
        for pattern in self.repos.iter().chain(&self.bases) {
            glob(pattern).with_context(|| format!("the glob {pattern:?}"))?;
        }
        ensure!(!self.outputs.is_empty(), "`outputs` lists no output type");
        for (output, limit) in &self.outputs {
            ensure!(
                OUTPUT_TYPES.contains(&output.as_str()),
                "the output type {output:?} is not one of {}",
                OUTPUT_TYPES.join(", ")
            );
            ensure!(limit.max >= 1, "`outputs.{output}.max` must be at least 1");
            // A hand-back holds one patch, for one pull request.
            ensure!(
                output != CREATE_PULL_REQUEST || limit.max == MAX_PULL_REQUESTS,
                "`outputs.{CREATE_PULL_REQUEST}.max` must be {MAX_PULL_REQUESTS}: a run hands back one patch"
            );
        }
        ensure!(self.max_outputs >= 1, "`max_outputs` must be at least 1");
        ensure!(
            self.max_patch_files >= 1,
            "`max_patch_files` must be at least 1"
        );
        ensure!(
            self.max_patch_bytes >= KB,
            "`max_patch_bytes` must be at least {KB}"
        );
        Ok(())
    }

    /// The protected file names for a patch to `repo`.
    fn protected_files(&self, repo: &str) -> Vec<String> {
        let unprotected = &self.unprotected_files;
        let exempt = unprotected
            .repos
            .iter()
            .any(|own| own.eq_ignore_ascii_case(repo));
        DEFAULT_PROTECTED_FILES
            .iter()
            .filter(|name| {
                !(exempt && unprotected.files.iter().any(|file| file.as_str() == **name))
            })
            .map(|name| (*name).to_owned())
            .collect()
    }

    /// Why `url` may not be cloned as `repo`, if it may not. The URL must
    /// name that repository on a host of the bounds and nothing more, so
    /// that the bounds on the repository are bounds on what is cloned.
    fn clone_url_problem(&self, url: &str, repo: &str) -> Option<String> {
        let Some((host, path)) = url
            .strip_prefix("https://")
            .and_then(|rest| rest.split_once('/'))
        else {
            return Some(format!("clone URL {url:?} is not https://HOST/OWNER/NAME"));
        };
        if !self
            .hosts
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(host))
        {
            return Some(format!(
                "the host of clone URL {url:?} is not within the bounds ({})",
                self.hosts.join(", ")
            ));
        }
        let path = path.strip_suffix(".git").unwrap_or(path);
        (!path.eq_ignore_ascii_case(repo))
            .then(|| format!("clone URL {url:?} does not name the repository {repo:?}"))
    }

    /// The policy of a run, or every reason its request is refused.
    ///
    /// The request's strings are untrusted. Each is matched against a
    /// pattern before it is used, the messages quote them escaped, and
    /// the policy is serialized by serde: a quote or a newline in one can
    /// neither add a key to `policy.json` nor a line to the log.
    pub fn compile(&self, request: &Request<'_>) -> Result<Policy, Vec<String>> {
        let Request {
            repo,
            clone_url,
            base,
            kind,
            outputs,
            max_outputs,
        } = *request;
        let mut errors = Vec::new();

        let dotted = |name: &str| name.split('/').any(|part| part == "." || part == "..");
        let repo_ok = REPO_RE.is_match(repo) && !dotted(repo);
        if !repo_ok || !glob_matches(&self.repos, repo) {
            errors.push(format!(
                "repo {repo:?} is not within the bounds ({})",
                self.repos.join(", ")
            ));
        }
        // Checked against the repository only if that is a name at all.
        if repo_ok && let Some(problem) = self.clone_url_problem(clone_url, repo) {
            errors.push(problem);
        }
        if !BASE_RE.is_match(base) || base.contains("..") || !glob_matches(&self.bases, base) {
            errors.push(format!(
                "base {base:?} is not within the bounds ({})",
                self.bases.join(", ")
            ));
        }

        // An analysis run has no change to propose.
        let no_change = kind == Kind::Analysis;
        let mut types: Vec<&str> = Vec::new();
        if outputs.trim() == ALL_OUTPUTS {
            types.extend(
                self.outputs
                    .keys()
                    .map(String::as_str)
                    .filter(|output| !(no_change && *output == CREATE_PULL_REQUEST)),
            );
        } else {
            for output in outputs.split(',').map(str::trim).filter(|o| !o.is_empty()) {
                if !types.contains(&output) {
                    types.push(output);
                }
            }
        }
        if types.is_empty() {
            errors.push("outputs lists no output type".to_owned());
        }
        let known = || {
            self.outputs
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        };
        errors.extend(
            types
                .iter()
                .filter(|output| !self.outputs.contains_key(**output))
                .map(|output| {
                    format!(
                        "output type {output:?} is not within the bounds ({})",
                        known()
                    )
                }),
        );
        if no_change && types.contains(&CREATE_PULL_REQUEST) {
            errors.push(format!(
                "an analysis run hands back no change, so it can't {CREATE_PULL_REQUEST}"
            ));
        }

        let max = match max_outputs {
            MAX_OUTPUTS => Some(self.max_outputs),
            // Digits only: `parse` alone would take a sign.
            n if !n.starts_with('0') && n.bytes().all(|b| b.is_ascii_digit()) => {
                n.parse::<u32>().ok().filter(|n| *n <= self.max_outputs)
            }
            _ => None,
        };
        if max.is_none() {
            errors.push(format!(
                "max_outputs {max_outputs:?} must be a number from 1 to {}",
                self.max_outputs
            ));
        }
        let Some(max) = max.filter(|_| errors.is_empty()) else {
            return Err(errors);
        };

        // The lower of what was asked for and what the bounds allow.
        let limit = |output: &str| OutputLimit {
            max: self
                .outputs
                .get(output)
                .map_or(0, |bound| bound.max)
                .min(max),
        };
        let safe_outputs = SafeOutputs {
            create_pull_request: types.contains(&CREATE_PULL_REQUEST).then(|| PullRequest {
                max: limit(CREATE_PULL_REQUEST).max,
                protected_files: self.protected_files(repo),
                protect_top_level_dot_folders: true,
                protected_files_policy: ProtectedFilesPolicy::Blocked,
                draft: true,
                max_patch_size: self.max_patch_bytes / KB,
                max_patch_files: self.max_patch_files,
            }),
            others: types
                .iter()
                .filter(|output| **output != CREATE_PULL_REQUEST)
                .map(|output| ((*output).to_owned(), limit(output)))
                .collect(),
        };
        Ok(Policy {
            repo: repo.to_owned(),
            clone_url: clone_url.to_owned(),
            base: base.to_owned(),
            kind,
            max_outputs: max,
            max_patch_bytes: self.max_patch_bytes,
            safe_outputs,
        })
    }
}

pub fn run(args: &Args) -> Result<Exit> {
    let bounds = Bounds::load(&args.allow)?;
    let request = Request {
        repo: &args.repo,
        clone_url: &args.clone_url,
        base: &args.base,
        kind: args.kind,
        outputs: &args.outputs,
        max_outputs: &args.max_outputs,
    };
    match bounds.compile(&request) {
        Ok(policy) => {
            let json = serde_json::to_string_pretty(&policy).context("writing the policy")?;
            println!("{json}");
            Ok(Exit::Success)
        }
        Err(errors) => {
            for error in errors {
                eprintln!("error: {error}");
            }
            Ok(Exit::Failure)
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The old tree's allowlist, as a bounds file.
    pub(crate) const BOUNDS: &str = r#"
hosts = ["github.com"]
repos = ["bootc-dev/*", "composefs/*", "cgwalters-bot/*", "cgwalters-forge/*"]
bases = ["main", "master", "bot/**"]
max_outputs = 5
max_patch_bytes = 8388608
max_patch_files = 100

[outputs]
create_pull_request = { max = 1 }
add_comment = { max = 3 }
noop = { max = 1 }
missing_tool = { max = 3 }
missing_data = { max = 3 }

[unprotected_files]
repos = ["cgwalters-bot/homegit", "cgwalters-forge/review"]
files = ["README.md", "AGENTS.md"]
"#;

    const REPO: &str = "bootc-dev/bootc";

    pub(crate) fn bounds() -> Bounds {
        let bounds: Bounds = toml::from_str(BOUNDS).unwrap();
        bounds.validate().unwrap();
        bounds
    }

    fn request() -> Request<'static> {
        Request {
            repo: REPO,
            clone_url: "https://github.com/bootc-dev/bootc",
            base: "main",
            kind: Kind::Branch,
            outputs: "create_pull_request,noop",
            max_outputs: "3",
        }
    }

    /// The policy of the request the tests of `check` use.
    pub(crate) fn policy() -> Policy {
        bounds().compile(&request()).unwrap()
    }

    #[test]
    fn requests_against_the_bounds() {
        let ok = request();
        let own = |repo: &'static str, clone_url: &'static str| Request {
            repo,
            clone_url,
            ..ok
        };
        // The name, the request, and a part of the refusal if it is refused.
        let cases: &[(&str, Request<'_>, Option<&str>)] = &[
            ("ok", ok, None),
            (
                "another owner",
                own("evil/bootc", "https://github.com/evil/bootc"),
                Some("repo \"evil/bootc\" is not within"),
            ),
            (
                "a repo of another case",
                own("BOOTC-DEV/bootc", "https://github.com/bootc-dev/bootc"),
                None,
            ),
            (
                "a malformed repo",
                Request {
                    repo: "bootc-dev/bootc/x",
                    ..ok
                },
                Some("repo "),
            ),
            (
                "a repo of dots",
                Request {
                    repo: "bootc-dev/..",
                    ..ok
                },
                Some("repo "),
            ),
            (
                "a base not listed",
                Request {
                    base: "release",
                    ..ok
                },
                Some("base \"release\""),
            ),
            (
                "a bot branch",
                Request {
                    base: "bot/agent-run-praxis",
                    ..ok
                },
                None,
            ),
            (
                "a base with ..",
                Request {
                    base: "bot/../main",
                    ..ok
                },
                Some("base "),
            ),
            (
                "a type not listed",
                Request {
                    outputs: "create_pull_request,delete_repo",
                    ..ok
                },
                Some("output type \"delete_repo\""),
            ),
            (
                "no types",
                Request {
                    outputs: " , ",
                    ..ok
                },
                Some("no output type"),
            ),
            (
                "too many outputs",
                Request {
                    max_outputs: "6",
                    ..ok
                },
                Some("max_outputs"),
            ),
            (
                "no number",
                Request {
                    max_outputs: "many",
                    ..ok
                },
                Some("max_outputs"),
            ),
            (
                "zero",
                Request {
                    max_outputs: "0",
                    ..ok
                },
                Some("max_outputs"),
            ),
            (
                "a signed number",
                Request {
                    max_outputs: "+2",
                    ..ok
                },
                Some("max_outputs"),
            ),
            (
                "no number at all",
                Request {
                    max_outputs: "",
                    ..ok
                },
                Some("max_outputs"),
            ),
            (
                "a pull request from an analysis run",
                Request {
                    kind: Kind::Analysis,
                    ..ok
                },
                Some("analysis"),
            ),
            (
                "an analysis run's comments",
                Request {
                    kind: Kind::Analysis,
                    outputs: "noop,add_comment",
                    ..ok
                },
                None,
            ),
            (
                "all types",
                Request {
                    outputs: "all",
                    max_outputs: "max",
                    ..ok
                },
                None,
            ),
            (
                "all types of an analysis run",
                Request {
                    kind: Kind::Analysis,
                    outputs: "all",
                    ..ok
                },
                None,
            ),
            (
                "a clone URL with .git",
                Request {
                    clone_url: "https://github.com/bootc-dev/bootc.git",
                    ..ok
                },
                None,
            ),
            (
                "a clone URL of another host",
                Request {
                    clone_url: "https://evil.example/bootc-dev/bootc",
                    ..ok
                },
                Some("host"),
            ),
            (
                "a clone URL of another repository",
                Request {
                    clone_url: "https://github.com/evil/bootc",
                    ..ok
                },
                Some("does not name"),
            ),
            (
                "a clone URL with a user",
                Request {
                    clone_url: "https://x@github.com/bootc-dev/bootc",
                    ..ok
                },
                Some("host"),
            ),
            (
                "a clone URL with a port",
                Request {
                    clone_url: "https://github.com:444/bootc-dev/bootc",
                    ..ok
                },
                Some("host"),
            ),
            (
                "a clone URL over ssh",
                Request {
                    clone_url: "ssh://github.com/bootc-dev/bootc",
                    ..ok
                },
                Some("is not https://"),
            ),
            (
                "a clone URL with more",
                Request {
                    clone_url: "https://github.com/bootc-dev/bootc/../x",
                    ..ok
                },
                Some("does not name"),
            ),
            (
                "a clone URL with a query",
                Request {
                    clone_url: "https://github.com/bootc-dev/bootc?x=1",
                    ..ok
                },
                Some("does not name"),
            ),
        ];
        let bounds = bounds();
        for (name, request, want) in cases {
            match (bounds.compile(request), want) {
                (Ok(_), None) => {}
                (Err(errors), Some(want)) => {
                    assert!(
                        errors.iter().any(|e| e.contains(want)),
                        "{name}: {errors:?}"
                    );
                }
                (result, _) => panic!("{name}: {result:?}"),
            }
        }
    }

    #[test]
    fn every_reason_is_given() {
        let request = Request {
            repo: "evil/x",
            clone_url: "https://github.com/evil/x",
            base: "release",
            kind: Kind::Analysis,
            outputs: "create_pull_request,delete_repo",
            max_outputs: "many",
        };
        let errors = bounds().compile(&request).unwrap_err();
        assert_eq!(errors.len(), 5, "{errors:?}");
    }

    #[test]
    fn ceilings_and_pull_request_settings() {
        let bounds = bounds();
        let compile = |request: Request<'_>| bounds.compile(&request).unwrap();

        let all = compile(Request {
            outputs: "all",
            max_outputs: "max",
            ..request()
        });
        assert_eq!(all.max_outputs, 5);
        assert_eq!(
            all.safe_outputs.types().collect::<Vec<_>>(),
            [
                "create_pull_request",
                "add_comment",
                "missing_data",
                "missing_tool",
                "noop"
            ]
        );
        // The lower of the bound and the request, for each type.
        for (max_outputs, output, want) in [
            ("max", "add_comment", 3),
            ("2", "add_comment", 2),
            ("2", "noop", 1),
            ("2", "create_pull_request", 1),
        ] {
            let policy = compile(Request {
                outputs: "all",
                max_outputs,
                ..request()
            });
            assert_eq!(
                policy.safe_outputs.max_of(output),
                Some(want),
                "{max_outputs} {output}"
            );
        }

        let analysis = compile(Request {
            kind: Kind::Analysis,
            outputs: "all",
            ..request()
        });
        assert_eq!(analysis.safe_outputs.create_pull_request, None);
        assert_eq!(analysis.safe_outputs.max_of("create_pull_request"), None);

        // Asked for twice, allowed once.
        let twice = compile(Request {
            outputs: "noop, noop ,create_pull_request",
            ..request()
        });
        assert_eq!(
            twice.safe_outputs.types().collect::<Vec<_>>(),
            ["create_pull_request", "noop"]
        );

        let pr = policy().safe_outputs.create_pull_request.unwrap();
        assert!(pr.draft);
        assert!(pr.protect_top_level_dot_folders);
        assert_eq!(pr.protected_files_policy, ProtectedFilesPolicy::Blocked);
        assert_eq!((pr.max_patch_size, pr.max_patch_files), (8192, 100));
    }

    #[test]
    fn docs_are_unprotected_only_where_the_bounds_say() {
        let bounds = bounds();
        let cases = [
            ("cgwalters-bot/homegit", true),
            ("CGWalters-Bot/Homegit", true),
            ("cgwalters-forge/review", true),
            (REPO, false),
            // Named exactly: a glob of the same owner is not enough.
            ("cgwalters-bot/bootc", false),
            ("cgwalters-forge/homegit", false),
        ];
        for (repo, own) in cases {
            let protected = bounds.protected_files(repo);
            let dropped: Vec<&str> = DEFAULT_PROTECTED_FILES
                .iter()
                .copied()
                .filter(|name| !protected.iter().any(|p| p.as_str() == *name))
                .collect();
            let want: &[&str] = if own {
                &["README.md", "AGENTS.md"]
            } else {
                &[]
            };
            assert_eq!(dropped, want, "{repo}");
        }
    }

    #[test]
    fn globs() {
        let cases = [
            ("bootc-dev/*", "bootc-dev/bootc", true),
            ("bootc-dev/*", "BOOTC-DEV/Bootc", true),
            ("bootc-dev/*", "bootc-dev/a/b", false),
            ("bootc-dev/*", "xbootc-dev/a", false),
            ("bot/**", "bot/a/b", true),
            ("bot/**", "bot", false),
            ("main", "main", true),
            ("main", "mainx", false),
            // Everything but the stars is itself.
            ("a.b", "axb", false),
            ("a+b[c]", "a+b[c]", true),
            ("ma(in", "ma(in", true),
        ];
        for (pattern, value, want) in cases {
            assert_eq!(
                glob_matches(&[pattern.to_owned()], value),
                want,
                "{pattern} {value}"
            );
        }
    }

    #[test]
    fn bad_bounds_files() {
        let with = |from: &str, to: &str| BOUNDS.replace(from, to);
        let cases = [
            (
                "an unknown key",
                format!("{BOUNDS}\nextra = 1\n"),
                "unknown field",
            ),
            (
                "an unknown output type",
                with("noop = ", "delete_repo = "),
                "delete_repo",
            ),
            (
                "no repositories",
                with(
                    "repos = [\"bootc-dev/*\", \"composefs/*\", \"cgwalters-bot/*\", \"cgwalters-forge/*\"]",
                    "repos = []",
                ),
                "`repos` is empty",
            ),
            (
                "no hosts",
                with("hosts = [\"github.com\"]", ""),
                "missing field `hosts`",
            ),
            (
                "two pull requests",
                with(
                    "create_pull_request = { max = 1 }",
                    "create_pull_request = { max = 2 }",
                ),
                "a run hands back one patch",
            ),
            (
                "a ceiling of zero",
                with("noop = { max = 1 }", "noop = { max = 0 }"),
                "at least 1",
            ),
            (
                "no room for a patch",
                with("max_patch_bytes = 8388608", "max_patch_bytes = 10"),
                "max_patch_bytes",
            ),
        ];
        for (name, text, want) in cases {
            let err = toml::from_str::<Bounds>(&text)
                .map_err(anyhow::Error::from)
                .and_then(|bounds| bounds.validate())
                .unwrap_err();
            assert!(format!("{err:#}").contains(want), "{name}: {err:#}");
        }
    }

    /// The concern of tracker#281: a request's text must not become
    /// structure. Whatever a field holds, the policy that comes back out
    /// of the JSON is the one that went in, with the same keys.
    #[test]
    fn a_field_cannot_add_a_key() {
        let hostile = [
            "main\",\"max_outputs\":99,\"x\":\"",
            "main\n\"max_outputs\": 99",
            "main\\\",\"safe_outputs\":{}",
            "main\u{0}\u{2028}'</script>",
        ];
        let bounds = bounds();
        let good = policy();
        let keys = |policy: &Policy| {
            let value: serde_json::Value = serde_json::to_value(policy).unwrap();
            value
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect::<Vec<_>>()
        };
        for text in hostile {
            // Refused where a field is the request's ...
            for request in [
                Request {
                    repo: text,
                    ..request()
                },
                Request {
                    clone_url: text,
                    ..request()
                },
                Request {
                    base: text,
                    ..request()
                },
                Request {
                    outputs: text,
                    ..request()
                },
                Request {
                    max_outputs: text,
                    ..request()
                },
            ] {
                assert!(bounds.compile(&request).is_err(), "{text:?}");
            }
            // ... and inert even in a policy that held it.
            let policy = Policy {
                repo: text.to_owned(),
                base: text.to_owned(),
                clone_url: text.to_owned(),
                ..good.clone()
            };
            let json = serde_json::to_string_pretty(&policy).unwrap();
            let back: Policy = serde_json::from_str(&json).unwrap();
            assert_eq!(back, policy, "{text:?}");
            assert_eq!(keys(&back), keys(&good), "{text:?}");
            assert_eq!(back.max_outputs, good.max_outputs);
        }
    }

    #[test]
    fn a_policy_that_policy_could_not_have_written_is_refused() {
        let good = policy();
        good.validate().unwrap();
        let pr = good.safe_outputs.create_pull_request.clone().unwrap();
        let with_pr = |pr: PullRequest| Policy {
            safe_outputs: SafeOutputs {
                create_pull_request: Some(pr),
                ..good.safe_outputs.clone()
            },
            ..good.clone()
        };
        let mut other_type = good.clone();
        other_type
            .safe_outputs
            .others
            .insert("create_issue".to_owned(), OutputLimit { max: 5 });
        let cases = [
            (
                "a type with no configuration here",
                other_type,
                "create_issue",
            ),
            (
                "a pull request that is not a draft",
                with_pr(PullRequest {
                    draft: false,
                    ..pr.clone()
                }),
                "not a draft",
            ),
            (
                "unprotected dot-folders",
                with_pr(PullRequest {
                    protect_top_level_dot_folders: false,
                    ..pr.clone()
                }),
                "dot-folders",
            ),
            (
                "two pull requests",
                with_pr(PullRequest {
                    max: 2,
                    ..pr.clone()
                }),
                "more than one",
            ),
        ];
        for (name, policy, want) in cases {
            let err = policy.validate().unwrap_err();
            assert!(format!("{err:#}").contains(want), "{name}: {err:#}");
        }
    }

    #[test]
    fn a_policy_that_does_not_refuse_protected_files_is_no_policy() {
        let json = serde_json::to_string(&policy()).unwrap();
        assert!(serde_json::from_str::<Policy>(&json).is_ok());
        for (from, to) in [
            ("\"blocked\"", "\"allowed\""),
            (
                "\"draft\":true",
                "\"draft\":true,\"allowed_files\":[\"**\"]",
            ),
            ("\"kind\":\"branch\"", "\"kind\":\"branch\",\"extra\":1"),
        ] {
            let changed = json.replace(from, to);
            assert_ne!(changed, json, "{from}");
            assert!(serde_json::from_str::<Policy>(&changed).is_err(), "{from}");
        }
    }
}
