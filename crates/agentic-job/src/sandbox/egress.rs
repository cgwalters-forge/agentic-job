//! The egress proxy: mitmproxy, with the addon and policy of `egress/` at
//! the top of this repository, installed and started by `sandbox setup`.
//!
//! It is the one piece that is not Rust. The policy needs the method and
//! path of every request, so the proxy has to end TLS, and a proxy that
//! ends TLS is security-critical code that is not written again here.
//! This module is the old tree's `egress-proxy.mjs start`: the proxy
//! runs as a system user of its own, in a transient service with no
//! capabilities and a read-only system, so the sandbox user can neither
//! signal it, read its certificate authority's key, nor change its
//! policy; and the network rules keep that user off everything private.
//!
//! Setup therefore needs Python, a package index and the threat feed's
//! host on the network.

use std::fs;
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};

use super::host::{self, User};
use crate::config::Egress;

/// The addon and what it needs, as they are in `egress/`: the binary
/// carries them, since it is all a runner gets.
const ADDON: &str = include_str!("../../../../egress/addon.py");

const POLICY_PY: &str = include_str!("../../../../egress/policy.py");

const POLICY_TOML: &str = include_str!("../../../../egress/policy.toml");

/// The pin of mitmproxy.
const REQUIREMENTS: &str = include_str!("../../../../egress/requirements.txt");

pub const EGRESS_USER: &str = "egress-proxy";

pub const PROXY_HOST: Ipv4Addr = Ipv4Addr::LOCALHOST;

pub const PROXY_PORT: u16 = 3128;

/// Root's: the virtual environment, and a copy of the addon and policy.
const INSTALL_DIR: &str = "/opt/egress-proxy";

const POLICY_DIR: &str = "/opt/egress-proxy/policy";

const DENYLIST: &str = "/opt/egress-proxy/denylist.txt";

const UNIT: &str = "egress-proxy.service";

/// The proxy's own (systemd's StateDirectory and LogsDirectory): its
/// certificate authority's key stays in the first, mode 0700.
const STATE_DIR: &str = "/var/lib/egress-proxy";

/// One JSON line per request: method, host, path and decision, never
/// headers, bodies or query strings.
pub const ACCESS_LOG: &str = "/var/log/egress-proxy/access.jsonl";

/// mitmproxy writes its authority into its configuration directory on
/// first start.
const CA_GENERATED: &str = "/var/lib/egress-proxy/mitmproxy-ca-cert.pem";

/// What the sandbox user trusts: the authority alone, and the system
/// bundle plus the authority for everything that replaces the bundle.
const PUBLIC_DIR: &str = "/etc/egress-proxy";

pub const CA_CERT: &str = "/etc/egress-proxy/ca.pem";

pub const CA_BUNDLE: &str = "/etc/egress-proxy/ca-bundle.pem";

const SYSTEM_BUNDLES: &[&str] = &[
    "/etc/pki/tls/certs/ca-bundle.crt",
    "/etc/ssl/certs/ca-certificates.crt",
];

const CERTIFICATE_MARKER: &str = "-----BEGIN CERTIFICATE-----";

/// HaGeZi's Threat Intelligence Feeds, medium: known malware, phishing
/// and command hosts, each with its subdomains. It lists none of the big
/// platforms, so it complements the write rules and does not replace
/// them. A fetch that fails leaves the feed empty, with a warning: it is
/// hardening, not the boundary.
const DENYLIST_URL: &str = "https://raw.githubusercontent.com/hagezi/dns-blocklists/main/wildcard/tif.medium-onlydomains.txt";

/// A feed shorter than this is not the feed.
const DENYLIST_MIN_ENTRIES: usize = 1000;

const FETCH_SECONDS: &str = "60";

const START_TIMEOUT: Duration = Duration::from_secs(60);

const START_POLL: Duration = Duration::from_millis(500);

const CONNECT_TIMEOUT: Duration = Duration::from_secs(1);

const MODE_FILE: u32 = 0o644;

const MODE_DIR: u32 = 0o755;

/// The unit's sandboxing: it needs only its state and log directories.
const UNIT_PROPERTIES: &[&str] = &[
    "User=egress-proxy",
    "Group=egress-proxy",
    "StateDirectory=egress-proxy",
    "StateDirectoryMode=0700",
    "LogsDirectory=egress-proxy",
    "LogsDirectoryMode=0750",
    "NoNewPrivileges=yes",
    "ProtectSystem=strict",
    "ProtectHome=yes",
    "PrivateTmp=yes",
    "PrivateDevices=yes",
    "ProtectKernelTunables=yes",
    "ProtectKernelModules=yes",
    "ProtectControlGroups=yes",
    "RestrictSUIDSGID=yes",
    "RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX",
    "CapabilityBoundingSet=",
    "LockPersonality=yes",
    "Restart=on-failure",
    "Environment=HOME=/var/lib/egress-proxy",
    "UMask=0077",
];

pub fn proxy_url() -> String {
    format!("http://{PROXY_HOST}:{PROXY_PORT}")
}

/// mitmdump's command line. It connects upstream only for a request the
/// policy allows; and without raw TCP, whatever goes through a tunnel is
/// parsed as HTTP, so other protocols fail and do not pass unseen.
fn mitmdump_args(denylist: bool) -> Vec<String> {
    let set = |option: &str, value: &str| ["--set".to_owned(), format!("{option}={value}")];
    [
        format!("{INSTALL_DIR}/bin/mitmdump"),
        "--mode".to_owned(),
        "regular".to_owned(),
        "--listen-host".to_owned(),
        PROXY_HOST.to_string(),
        "--listen-port".to_owned(),
        PROXY_PORT.to_string(),
    ]
    .into_iter()
    .chain(set("confdir", STATE_DIR))
    .chain(set("connection_strategy", "lazy"))
    .chain(set("rawtcp", "false"))
    .chain(set("flow_detail", "0"))
    .chain(set("termlog_verbosity", "warn"))
    .chain(["-s".to_owned(), format!("{POLICY_DIR}/addon.py")])
    .chain(set("egress_policy", &format!("{POLICY_DIR}/policy.toml")))
    .chain(set("egress_denylist", if denylist { DENYLIST } else { "" }))
    .chain(set("egress_log", ACCESS_LOG))
    .collect()
}

fn write(path: &str, content: &str) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::write(path, content).with_context(|| format!("writing {path}"))?;
    fs::set_permissions(path, fs::Permissions::from_mode(MODE_FILE))
        .with_context(|| format!("setting the mode of {path}"))
}

fn make_dir(path: &str) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::create_dir_all(path).with_context(|| format!("creating {path}"))?;
    fs::set_permissions(path, fs::Permissions::from_mode(MODE_DIR))
        .with_context(|| format!("setting the mode of {path}"))
}

/// The proxy's user, the virtual environment with the pinned mitmproxy,
/// and root's copy of the addon and the policy. `policy` replaces the
/// write rules that come with the binary.
fn install(policy: Option<&Path>) -> Result<()> {
    if User::lookup(EGRESS_USER)?.is_none() {
        host::run(Command::new("useradd").args([
            "--system",
            "--no-create-home",
            "--home-dir",
            STATE_DIR,
            "--shell",
            "/sbin/nologin",
            EGRESS_USER,
        ]))?;
    }
    host::run(Command::new("python3").args(["-m", "venv", INSTALL_DIR])).context(
        "making the egress proxy's Python environment (is python3's venv module installed?)",
    )?;
    let requirements = format!("{INSTALL_DIR}/requirements.txt");
    write(&requirements, REQUIREMENTS)?;
    host::run(Command::new(format!("{INSTALL_DIR}/bin/pip")).args([
        "install",
        "--quiet",
        "--disable-pip-version-check",
        "-r",
        &requirements,
    ]))
    .context("installing mitmproxy")?;
    make_dir(POLICY_DIR)?;
    let rules = match policy {
        Some(path) => fs::read_to_string(path)
            .with_context(|| format!("reading egress.policy {}", path.display()))?,
        None => POLICY_TOML.to_owned(),
    };
    for (name, content) in [
        ("addon.py", ADDON),
        ("policy.py", POLICY_PY),
        ("policy.toml", rules.as_str()),
    ] {
        write(&format!("{POLICY_DIR}/{name}"), content)?;
    }
    Ok(())
}

/// How many domains a feed's text holds.
fn feed_entries(text: &str) -> usize {
    text.lines()
        .filter(|line| !line.trim().is_empty() && !line.starts_with('#'))
        .count()
}

/// Fetches the threat feed; whether there is one.
fn fetch_denylist() -> bool {
    let fetched = host::run(Command::new("curl").args([
        "--fail",
        "--silent",
        "--show-error",
        "--location",
        "--max-time",
        FETCH_SECONDS,
        DENYLIST_URL,
    ]))
    .and_then(|text| {
        let entries = feed_entries(&text);
        ensure!(entries >= DENYLIST_MIN_ENTRIES, "only {entries} entries");
        write(DENYLIST, &text)?;
        Ok(entries)
    });
    match fetched {
        Ok(entries) => {
            println!("Threat feed: {entries} domains from {DENYLIST_URL}");
            true
        }
        Err(err) => {
            eprintln!(
                "warning: the egress proxy runs without a threat feed: fetching {DENYLIST_URL} failed ({err:#})"
            );
            false
        }
    }
}

fn listening() -> bool {
    let addr = SocketAddr::from((PROXY_HOST, PROXY_PORT));
    TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT).is_ok()
}

/// Waits until the proxy listens and has written its authority.
fn wait_started() -> Result<()> {
    let deadline = Instant::now() + START_TIMEOUT;
    while Instant::now() < deadline {
        if Path::new(CA_GENERATED).exists() && listening() {
            return Ok(());
        }
        if !host::succeeds(Command::new("systemctl").args(["is-active", "--quiet", UNIT])) {
            break;
        }
        std::thread::sleep(START_POLL);
    }
    // Why it did not start is in its journal.
    let _ = Command::new("journalctl")
        .args(["-u", UNIT, "--no-pager", "-n", "50"])
        .status();
    bail!("{UNIT} didn't start listening on {}", proxy_url())
}

/// Publishes the authority's certificate, alone and after the system
/// bundle.
fn publish_ca() -> Result<()> {
    let system = SYSTEM_BUNDLES
        .iter()
        .find(|path| Path::new(path).exists())
        .with_context(|| format!("no system CA bundle ({})", SYSTEM_BUNDLES.join(", ")))?;
    let ca = fs::read_to_string(CA_GENERATED).with_context(|| format!("reading {CA_GENERATED}"))?;
    ensure!(
        ca.contains(CERTIFICATE_MARKER),
        "{CA_GENERATED} holds no certificate"
    );
    let bundle = fs::read_to_string(system).with_context(|| format!("reading {system}"))?;
    make_dir(PUBLIC_DIR)?;
    write(CA_CERT, &ca)?;
    write(CA_BUNDLE, &format!("{}\n{ca}", bundle.trim_end()))
}

/// Installs and starts the proxy; returns its uid, which the network
/// rules name.
pub fn start(egress: &Egress) -> Result<u32> {
    install(egress.policy.as_deref())?;
    let denylist = fetch_denylist();
    let mut run = Command::new("systemd-run");
    run.arg(format!("--unit={UNIT}"))
        .args(["--service-type=exec", "--collect"])
        .args(
            UNIT_PROPERTIES
                .iter()
                .map(|property| format!("--property={property}")),
        )
        .arg("--")
        .args(mitmdump_args(denylist));
    host::run(&mut run).context("starting the egress proxy")?;
    wait_started()?;
    publish_ca()?;
    let user = User::lookup(EGRESS_USER)?.with_context(|| format!("no user {EGRESS_USER}"))?;
    println!(
        "Egress proxy listening on {} as {EGRESS_USER}; its certificate authority is {CA_CERT}",
        proxy_url()
    );
    Ok(user.uid)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The old tree's command line, argument for argument.
    #[test]
    fn mitmdump_command_line() {
        let want = "/opt/egress-proxy/bin/mitmdump --mode regular --listen-host 127.0.0.1 --listen-port 3128 \
                    --set confdir=/var/lib/egress-proxy --set connection_strategy=lazy --set rawtcp=false \
                    --set flow_detail=0 --set termlog_verbosity=warn -s /opt/egress-proxy/policy/addon.py \
                    --set egress_policy=/opt/egress-proxy/policy/policy.toml \
                    --set egress_denylist=/opt/egress-proxy/denylist.txt \
                    --set egress_log=/var/log/egress-proxy/access.jsonl";
        assert_eq!(mitmdump_args(true).join(" "), want);
        assert!(mitmdump_args(false).contains(&"egress_denylist=".to_owned()));
    }

    #[test]
    fn the_unit_runs_as_the_proxy_user_in_its_own_directories() {
        for (property, value) in [
            ("User", EGRESS_USER),
            ("Group", EGRESS_USER),
            ("StateDirectory", EGRESS_USER),
            ("LogsDirectory", EGRESS_USER),
            ("Environment", "HOME=/var/lib/egress-proxy"),
            ("CapabilityBoundingSet", ""),
        ] {
            let want = format!("{property}={value}");
            assert!(UNIT_PROPERTIES.contains(&want.as_str()), "{want}");
        }
        assert!(STATE_DIR.ends_with(EGRESS_USER) && ACCESS_LOG.contains(EGRESS_USER));
    }

    #[test]
    fn feed_lines() {
        assert_eq!(feed_entries("# a feed\nevil.example\n\n  \nbad.test\n"), 2);
        assert_eq!(feed_entries(""), 0);
    }

    #[test]
    fn the_pin_and_the_files_come_with_the_binary() {
        assert!(
            REQUIREMENTS
                .lines()
                .any(|line| line.starts_with("mitmproxy=="))
        );
        assert!(ADDON.contains("X-Egress-Denied"));
        assert!(POLICY_PY.contains("class Policy"));
        assert!(POLICY_TOML.contains("git-upload-pack"));
    }
}
