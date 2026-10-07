//! Each probe of `sandbox check` fails when its protection is removed.
//!
//! This changes the host it runs on, as root through sudo, and puts each
//! thing back: it is for a machine that is thrown away, after `sandbox
//! setup` has run there. CI's `sandbox` job runs it:
//!
//! ```text
//! cargo test --test sandbox_host -- --ignored --nocapture
//! ```
//!
//! The run-token probes want the job's identity-token request variables
//! in the environment (any values: the probes only look for them where
//! they must not be).

// Helpers outside a #[test] function are still test code.
#![allow(clippy::unwrap_used)]

use std::any::Any;
use std::collections::BTreeSet;
use std::io::Write;
use std::net::{Ipv4Addr, TcpListener};
use std::os::linux::net::SocketAddrExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{SocketAddr as UnixAddr, UnixListener};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use agentic_job::config::{self, Config};
use agentic_job::sandbox::check::{self, RunToken};
use agentic_job::sandbox::enter::Entry;
use agentic_job::sandbox::host::User;
use agentic_job::sandbox::{egress, network, setup};

const BIN: &str = env!("CARGO_BIN_EXE_agentic-job");

/// polkitd watches its rules directory; give it a moment to reload.
const POLKIT_SETTLE: Duration = Duration::from_secs(3);

const POLKIT_DENY: &str = "/etc/polkit-1/rules.d/00-agentic-job-sandbox.rules";

const POLKIT_ASIDE: &str = "/etc/polkit-1/agentic-job-sandbox.rules.aside";

/// Sorts before the deny rule, so the first rule to answer is this one.
const POLKIT_GRANT: &str = "/etc/polkit-1/rules.d/00-aaa-test-grant.rules";

/// Sorts after the deny rule: the last matching sudoers rule wins.
const SUDOERS_GRANT: &str = "/etc/sudoers.d/zzz-test-grant";

const WORLD_WRITABLE: &str = "/etc/agentic-job-test-world-writable";

const TAILSCALE_DIR: &str = "/var/run/tailscale";

const TAILSCALE_SOCKET: &str = "/var/run/tailscale/tailscaled.sock";

const HCA_DIR: &str = "/opt/hca";

const SUDOERS_EARLY_GRANT: &str = "/etc/sudoers.d/aa-test-grant";

/// A group both runner images have, and that reads the host's logs.
const FORBIDDEN_GROUP: &str = "adm";

const READABLE_TOKEN: &str = "/etc/agentic-job-test-token";

/// The variable whose value the job must keep from the sandbox user.
const REQUEST_TOKEN_VAR: &str = "ACTIONS_ID_TOKEN_REQUEST_TOKEN";

/// The working directory changed to one outside the runner's home; put
/// back when dropped.
struct WorkDir {
    before: PathBuf,
    dir: PathBuf,
}

impl WorkDir {
    fn enter(dir: &str) -> Self {
        let before = std::env::current_dir().unwrap();
        std::fs::create_dir(dir).unwrap();
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::env::set_current_dir(dir).unwrap();
        Self {
            before,
            dir: PathBuf::from(dir),
        }
    }
}

impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = std::env::set_current_dir(&self.before);
        let _ = std::fs::remove_dir(&self.dir);
    }
}

/// A process of the sandbox user's that runs `script` (sh, with `arg` as
/// `$1`) until the returned value is dropped. The script must sleep.
fn sandbox_process(entry: &Entry, uid: u32, script: &'static str, arg: String) -> Removed {
    let entry = entry.clone();
    let holder = std::thread::spawn(move || {
        let _ = entry.run(&["sh", "-c", script, "sh", &arg], b"");
    });
    std::thread::sleep(Duration::from_secs(3));
    let stop = strings(&["pkill", "-KILL", "-u", &uid.to_string(), "sleep"]);
    Removed::new(vec![stop], vec![Box::new(holder)])
}

/// A stand-in the length and shape of a real run token.
const TOKEN: &str = "test-run-0123456789abcdef0123456789abcdef0123456789abcdef";

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|&item| item.to_owned()).collect()
}

/// Runs `argv` as root; whether it succeeded.
fn root_ok(argv: &[String]) -> bool {
    Command::new("sudo")
        .arg("-n")
        .arg("--")
        .args(argv)
        .stdin(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn root(argv: &[&str]) {
    assert!(root_ok(&strings(argv)), "sudo {argv:?} failed");
}

/// Runs `argv` as root with `input` on its standard input.
fn root_input(argv: &[&str], input: &str) {
    let mut child = Command::new("sudo")
        .arg("-n")
        .arg("--")
        .args(argv)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    assert!(child.wait().unwrap().success(), "sudo {argv:?} failed");
}

/// Writes `content` to `path` as root, with `mode`.
fn root_write(path: &str, content: &str, mode: &str) {
    root_input(&["tee", "--", path], content);
    root(&["chmod", mode, path]);
}

/// What must fail with no network rules at all, on CI's hosted runner:
/// Azure, whose metadata service the runner reaches, with a local
/// resolver on loopback. Its WireServer does not answer HTTP there even
/// for the runner, so those two probes have nothing to lose.
const NO_RULES: &[&str] = &[
    "metadata",
    "container-metadata",
    "egress-direct",
    "egress-tcp",
    "egress-resolve",
    "egress-dns",
    "egress-proxy-metadata",
    "egress-container-direct",
    "egress-container-own-network",
    "local-tcp",
];

const NO_EGRESS_RULES: &[&str] = &[
    "egress-direct",
    "egress-tcp",
    "egress-resolve",
    "egress-dns",
    "egress-proxy-metadata",
    "egress-container-direct",
    "egress-container-own-network",
    "local-tcp",
];

const PROXY_UNRESTRICTED: &[&str] = &["egress-proxy-metadata"];

/// The expected failures of a network case: `default`, or the
/// comma-separated list in `AGENTIC_JOB_TEST_<NAME>` for a host whose
/// network differs (no local resolver, a tailnet with direct endpoints).
fn network_case(name: &str, default: &'static [&'static str]) -> Vec<&'static str> {
    match std::env::var(format!("AGENTIC_JOB_TEST_{name}")) {
        // Leaked: a handful of names, for the life of one test run.
        Ok(list) => list
            .split(',')
            .map(|id| &*Box::leak(id.to_owned().into_boxed_str()))
            .collect(),
        Err(_) => default.to_vec(),
    }
}

/// A protection that has been removed. Dropping it puts the host back.
struct Removed {
    restore: Vec<Vec<String>>,
    settle: Duration,
    held: Vec<Box<dyn Any>>,
}

impl Removed {
    /// `restore` is run as root on drop, after `held` is let go.
    fn new(restore: Vec<Vec<String>>, held: Vec<Box<dyn Any>>) -> Self {
        Self {
            restore,
            settle: Duration::ZERO,
            held,
        }
    }

    /// Removed by running `apply` as root, put back by `restore`.
    fn by(apply: &[&[&str]], restore: &[&[&str]]) -> Self {
        apply.iter().for_each(|argv| root(argv));
        Self::new(
            restore.iter().map(|argv| strings(argv)).collect(),
            Vec::new(),
        )
    }

    /// Waits now, and again once it is put back.
    fn settling(mut self, settle: Duration) -> Self {
        std::thread::sleep(settle);
        self.settle = settle;
        self
    }
}

impl Drop for Removed {
    fn drop(&mut self) {
        self.held.clear();
        for argv in &self.restore {
            if !root_ok(argv) {
                eprintln!("warning: could not put back: sudo {argv:?}");
            }
        }
        std::thread::sleep(self.settle);
    }
}

/// Whether the probes get a run token, and whose copy of it.
enum Token {
    None,
    /// The fixture's: the runner's file in its closed home.
    InPlace,
    /// The runner's copy somewhere else.
    RunnerFile(PathBuf),
}

struct Case {
    name: &'static str,
    /// The ids that must fail, and no others.
    expect: Vec<String>,
    token: Token,
    remove: Box<dyn FnOnce() -> Removed>,
}

impl Case {
    fn new(
        name: &'static str,
        expect: &[&str],
        remove: impl FnOnce() -> Removed + 'static,
    ) -> Self {
        Self {
            name,
            expect: strings(expect),
            token: Token::None,
            remove: Box::new(remove),
        }
    }

    fn with_token(self) -> Self {
        Self {
            token: Token::InPlace,
            ..self
        }
    }

    fn with_runner_file(self, path: &str) -> Self {
        Self {
            token: Token::RunnerFile(PathBuf::from(path)),
            ..self
        }
    }
}

/// The run token where `run` would put it: the runner's own file, and the
/// agent's configuration in the sandbox user's home.
struct TokenFixture {
    runner_file: PathBuf,
    agent_dir: PathBuf,
    agent_config: PathBuf,
}

impl TokenFixture {
    fn create(runner: &User, entry: &Entry) -> Self {
        let runner_file = runner.home.join(".agentic-job-test-token");
        std::fs::write(&runner_file, format!("{TOKEN}\n")).unwrap();
        std::fs::set_permissions(&runner_file, std::fs::Permissions::from_mode(0o600)).unwrap();
        let agent_dir = entry.home().join("agent-config");
        let agent_config = agent_dir.join("settings.json");
        // The modes are set outright, as `run` must: what a umask gives
        // is the host's business.
        let script = "mkdir -p \"$1\" && chmod 700 \"$1\" && cat > \"$2\" && chmod 600 \"$2\"";
        let written = entry
            .run(
                &[
                    "sh",
                    "-c",
                    script,
                    "sh",
                    &agent_dir.display().to_string(),
                    &agent_config.display().to_string(),
                ],
                format!("{{\"token\": \"{TOKEN}\"}}\n").as_bytes(),
            )
            .unwrap();
        assert!(
            written.success(),
            "{}",
            String::from_utf8_lossy(&written.stderr)
        );
        Self {
            runner_file,
            agent_dir,
            agent_config,
        }
    }

    fn token(&self) -> RunToken<'_> {
        RunToken {
            runner_file: &self.runner_file,
            agent_config: &self.agent_config,
        }
    }
}

/// Runs `script` (sh) as the sandbox user; it must succeed.
fn in_sandbox(entry: &Entry, script: &str) {
    let output = entry.run(&["sh", "-c", script], b"").unwrap();
    assert!(
        output.success(),
        "{script}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn cases(config: &Config, runner: &User, entry: &Entry, fixture: &TokenFixture) -> Vec<Case> {
    let user = config.sandbox.user.clone();
    let uid = entry.user().uid;
    let home = runner.home.display().to_string();
    let mut cases = Vec::new();

    {
        let user = user.clone();
        cases.push(Case::new(
            "a sudoers rule for the sandbox user",
            &["sudo"],
            move || {
                root_write(
                    SUDOERS_GRANT,
                    &format!("{user} ALL=(ALL:ALL) NOPASSWD: ALL\n"),
                    "0440",
                );
                Removed::by(&[], &[&["rm", "-f", SUDOERS_GRANT]])
            },
        ));
    }
    {
        // The deny rule is read last, so a grant read before it loses.
        let user = user.clone();
        cases.push(Case::new(
            "nothing: a sudoers rule that sorts before the deny rule",
            &[],
            move || {
                let grant = format!("{user} ALL=(ALL:ALL) NOPASSWD: ALL\n");
                root_write(SUDOERS_EARLY_GRANT, &grant, "0440");
                Removed::by(&[], &[&["rm", "-f", SUDOERS_EARLY_GRANT]])
            },
        ));
    }
    {
        let user = user.clone();
        cases.push(Case::new(
            "a group that reads the host's logs",
            &["groups"],
            move || {
                Removed::by(
                    &[&["usermod", "-aG", FORBIDDEN_GROUP, &user]],
                    &[&["gpasswd", "-d", &user, FORBIDDEN_GROUP]],
                )
            },
        ));
    }
    // Enabling one's own lingering is open to every user by default, so
    // the deny rule alone is what stops it; pkexec needs a grant as well.
    cases.push(Case::new("the polkit deny rule", &["linger"], || {
        Removed::by(
            &[&["mv", POLKIT_DENY, POLKIT_ASIDE]],
            &[&["mv", POLKIT_ASIDE, POLKIT_DENY]],
        )
        .settling(POLKIT_SETTLE)
    }));
    {
        let user = user.clone();
        cases.push(Case::new(
            "the polkit deny rule, with a rule that grants",
            &["polkit", "linger"],
            move || {
                let grant = format!(
                    "polkit.addRule(function(action, subject) {{\n  if (subject.user == \"{user}\") return polkit.Result.YES;\n}});\n"
                );
                root_write(POLKIT_GRANT, &grant, "0644");
                Removed::by(
                    &[&["mv", POLKIT_DENY, POLKIT_ASIDE]],
                    &[&["rm", "-f", POLKIT_GRANT], &["mv", POLKIT_ASIDE, POLKIT_DENY]],
                )
                .settling(POLKIT_SETTLE)
            },
        ));
    }
    {
        let home = home.clone();
        cases.push(Case::new(
            "the runner's home open",
            &[&format!("private-dir:{home}")],
            move || Removed::by(&[&["chmod", "0755", &home]], &[&["chmod", "0700", &home]]),
        ));
    }
    {
        // Its names are hidden and every file in it can still be opened.
        let home = home.clone();
        cases.push(Case::new(
            "the runner's home searchable",
            &[&format!("private-dir:{home}")],
            move || Removed::by(&[&["chmod", "0711", &home]], &[&["chmod", "0700", &home]]),
        ));
    }
    {
        let work = format!("/var/tmp/agentic-job-test-work-{}", std::process::id());
        let expect = format!("private-dir:{work}");
        cases.push(Case::new(
            "the job's work directory outside the runner's home",
            &[&expect],
            move || Removed::new(Vec::new(), vec![Box::new(WorkDir::enter(&work))]),
        ));
    }
    {
        // The value of the variable that asks for the job's identity
        // token, in the environment of a process of the sandbox user's.
        let entry = entry.clone();
        let value = std::env::var(REQUEST_TOKEN_VAR)
            .expect("the identity-token request variables must be set for this test");
        cases.push(Case::new(
            "the identity-token request token in a sandbox process",
            &[&format!("oidc-value:{REQUEST_TOKEN_VAR}")],
            move || sandbox_process(&entry, uid, "export LEAK=\"$1\"; sleep 600; true", value),
        ));
    }
    {
        // What the system manager has in its environment, every service
        // it starts gets, the sandbox user's sessions among them.
        let user = user.clone();
        cases.push(Case::new(
            "a job variable in the system manager's environment",
            &["env-job-variables", "environ-job-variables"],
            move || {
                Removed::by(
                    &[&["systemctl", "set-environment", "ACTIONS_LEAK_TEST=1"]],
                    &[
                        &["systemctl", "unset-environment", "ACTIONS_LEAK_TEST"],
                        // Its user manager started with the variable.
                        &["loginctl", "terminate-user", &user],
                    ],
                )
            },
        ));
    }
    for (name, id, file) in [
        ("cron", "cron", "/etc/cron.deny"),
        ("at", "at", "/etc/at.deny"),
    ] {
        let user = user.clone();
        cases.push(Case::new(name, &[id], move || {
            let delete = format!("/^{user}$/d");
            let append = format!("echo {user} >> {file}");
            Removed::by(&[&["sed", "-i", &delete, file]], &[&["sh", "-c", &append]])
        }));
    }
    cases.push(Case::new(
        "a world-writable file in /etc",
        &["world-writable"],
        || {
            root_write(WORLD_WRITABLE, "", "0666");
            Removed::by(&[], &[&["rm", "-f", WORLD_WRITABLE]])
        },
    ));
    cases.push(Case::new(
        "ptrace of any process of one's uid",
        &["ptrace-scope"],
        || {
            Removed::by(
                &[&["sysctl", "-q", "-w", "kernel.yama.ptrace_scope=0"]],
                &[&["sysctl", "-q", "-w", "kernel.yama.ptrace_scope=1"]],
            )
        },
    ));
    let unit = config
        .sandbox
        .stop_services
        .iter()
        .find(|unit| root_ok(&strings(&["systemctl", "cat", "--", unit])))
        .expect("the configuration must stop a unit this host has, for the test to start again")
        .clone();
    cases.push(Case::new(
        "a stopped service started",
        &[&format!("service:{unit}")],
        move || {
            Removed::by(
                &[&["systemctl", "start", &unit]],
                &[&["systemctl", "stop", &unit]],
            )
        },
    ));
    cases.push(Case::new(
        "listeners any user can connect to",
        &["local-sockets", "local-tcp"],
        || {
            let name = format!("agentic-job-test-listener-{}", std::process::id());
            let unix =
                UnixListener::bind_addr(&UnixAddr::from_abstract_name(name).unwrap()).unwrap();
            let tcp = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
            Removed::new(Vec::new(), vec![Box::new(unix), Box::new(tcp)])
        },
    ));
    if Path::new(HCA_DIR).is_dir() {
        cases.push(Case::new(
            "the hosted compute agent's directory open",
            &[&format!("private-dir:{HCA_DIR}")],
            || {
                Removed::by(
                    &[&["chmod", "0755", HCA_DIR]],
                    &[&["chmod", "0700", HCA_DIR]],
                )
            },
        ));
    }
    if root_ok(&strings(&["test", "-S", TAILSCALE_SOCKET])) {
        cases.push(Case::new(
            "tailscaled's socket directory open",
            &[
                &format!("private-dir:{TAILSCALE_DIR}"),
                "local-sockets",
                "tailscale-status",
                "tailscale-localapi",
                // Dialing through tailscaled is only tried for a host
                // with direct endpoints.
                if config.egress.direct.is_empty() {
                    "tailscale-status"
                } else {
                    "tailnet-nc"
                },
            ],
            || {
                Removed::by(
                    &[&["chmod", "0755", TAILSCALE_DIR]],
                    &[&["chmod", "0700", TAILSCALE_DIR]],
                )
            },
        ));
    }

    // The network rules. Each case loads other rules in their place and
    // puts back the ones setup left.
    let rules_back: &[&[&str]] = &[&["nft", "-f", network::RULES_FILE]];
    let uids: Vec<String> = std::iter::once(uid.to_string())
        .chain(
            setup::subuid_ranges(
                entry.user(),
                &std::fs::read_to_string("/etc/subuid").unwrap(),
            )
            .unwrap(),
        )
        .collect();
    if config.egress.proxy {
        let direct = network::direct(&config.egress).unwrap();
        let proxy_uid = User::lookup(egress::EGRESS_USER).unwrap().unwrap().uid;
        cases.push(Case::new(
            "the network rules",
            &network_case("NO_RULES", NO_RULES),
            move || {
                Removed::by(
                    &[&["nft", "delete", "table", "inet", network::TABLE]],
                    rules_back,
                )
            },
        ));
        {
            // The rules of a host without the proxy: the metadata service
            // stays closed and every way around the proxy opens.
            let (uids, direct) = (uids.clone(), direct.clone());
            cases.push(Case::new(
                "the rules that send everything through the proxy",
                &network_case("NO_EGRESS_RULES", NO_EGRESS_RULES),
                move || {
                    root_input(&["nft", "-f", "-"], &network::rules(&uids, &direct, None));
                    Removed::by(&[], rules_back)
                },
            ));
        }
        {
            // Rules that name another uid as the proxy's: the proxy
            // itself is then free to go where the sandbox may not.
            cases.push(Case::new(
                "the rules on the proxy's own user",
                &network_case("PROXY_UNRESTRICTED", PROXY_UNRESTRICTED),
                move || {
                    let other = Some(proxy_uid + 1);
                    root_input(&["nft", "-f", "-"], &network::rules(&uids, &direct, other));
                    Removed::by(&[], rules_back)
                },
            ));
        }
    }

    // The run token.
    let dir = fixture.agent_dir.display().to_string();
    let file = fixture.agent_config.display().to_string();
    cases.push(
        Case::new("the run token in place", &[], || {
            Removed::new(Vec::new(), Vec::new())
        })
        .with_token(),
    );
    {
        let (entry, file) = (entry.clone(), file.clone());
        cases.push(
            Case::new(
                "the agent's configuration readable by all",
                &["token-config-mode"],
                move || {
                    in_sandbox(&entry, &format!("chmod 644 {file}"));
                    Removed::by(&[], &[&["chmod", "600", &file]])
                },
            )
            .with_token(),
        );
    }
    {
        // Now a subordinate uid, any container's user, reads the token.
        let (entry, dir, file) = (entry.clone(), dir.clone(), file.clone());
        cases.push(
            Case::new(
                "the agent's configuration and its directory readable by all",
                &[
                    "token-config-mode",
                    "token-config-dir-mode",
                    "token-container-subuid",
                ],
                move || {
                    in_sandbox(&entry, &format!("chmod 755 {dir} && chmod 644 {file}"));
                    Removed::by(&[], &[&["chmod", "700", &dir], &["chmod", "600", &file]])
                },
            )
            .with_token(),
        );
    }
    {
        let entry = entry.clone();
        let copy = format!("/tmp/agentic-job-test-token-copy-{uid}");
        let source = file.clone();
        cases.push(
            Case::new("a copy of the token in /tmp", &["token-files"], move || {
                in_sandbox(&entry, &format!("cp {source} {copy}"));
                Removed::by(&[], &[&["rm", "-f", &copy]])
            })
            .with_token(),
        );
    }
    {
        // A process of the sandbox user's with the token as an argument,
        // alive while the probes run.
        let entry = entry.clone();
        cases.push(
            Case::new(
                "the token on a command line",
                &["token-processes"],
                move || sandbox_process(&entry, uid, "sleep 600; true", TOKEN.to_owned()),
            )
            .with_token(),
        );
    }
    cases.push(
        Case::new(
            "the runner's copy of the token where anyone reads it",
            &["token-runner-file"],
            || {
                root_write(READABLE_TOKEN, &format!("{TOKEN}\n"), "0644");
                Removed::by(&[], &[&["rm", "-f", READABLE_TOKEN]])
            },
        )
        .with_runner_file(READABLE_TOKEN),
    );
    cases
}

fn failed(config: &Config, token: Option<RunToken<'_>>) -> BTreeSet<String> {
    let report = check::probes(config, token).expect("the probes could not run");
    report.failures().into_iter().map(str::to_owned).collect()
}

#[test]
#[ignore = "changes the host as root; CI's sandbox job runs it after `sandbox setup`"]
fn each_probe_fails_when_its_protection_is_removed() {
    let config =
        Config::load(Path::new(config::ROOT_COPY)).expect("`sandbox setup` has not run here");
    let runner = User::current().unwrap();
    let entry = Entry::new(&config).unwrap();
    let none = BTreeSet::new();

    assert_eq!(failed(&config, None), none, "before anything is removed");

    // A machine is set up once: a second job must not inherit the first's
    // sandbox user.
    let again = Command::new("sudo")
        .args([
            "-n",
            "--",
            BIN,
            "sandbox",
            "setup",
            "--config",
            config::ROOT_COPY,
        ])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&again.stderr);
    assert_eq!(again.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("ran on this machine before"), "{stderr}");

    let fixture = TokenFixture::create(&runner, &entry);
    let mut wrong = Vec::new();
    for case in cases(&config, &runner, &entry, &fixture) {
        println!("\n=== removed: {} ===", case.name);
        let token = match &case.token {
            Token::None => None,
            Token::InPlace => Some(fixture.token()),
            Token::RunnerFile(path) => Some(RunToken {
                runner_file: path,
                agent_config: &fixture.agent_config,
            }),
        };
        let removed = (case.remove)();
        let got = failed(&config, token);
        drop(removed);
        let want: BTreeSet<String> = case.expect.into_iter().collect();
        if got != want {
            wrong.push(format!("{}: failed {got:?}, expected {want:?}", case.name));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));

    println!("\n=== everything put back ===");
    assert_eq!(
        failed(&config, Some(fixture.token())),
        none,
        "after everything was put back"
    );
}
