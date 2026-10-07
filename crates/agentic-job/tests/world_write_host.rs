//! What a non-root user can and cannot write on a host after `sandbox
//! setup`, with world write closed by the walk or by the BPF program
//! (docs/world-write.md). Run as the runner's user, which has no root
//! after setup; root's side goes through the same shell behind a socket
//! as `sandbox_host.rs` uses, started before setup. `#[ignore]`: it
//! changes the host.
//!
//! `AGENTIC_JOB_WORLD_WRITE` says which mode setup ran in: `walk` or
//! `lsm`. The two agree on everything but what is made world-writable
//! after setup, which only the program denies.

#![allow(clippy::unwrap_used)]

use std::io::{Read, Write};
use std::net::Shutdown;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::process::Command;

const ROOT_SOCKET: &str = "/run/agentic-job-test-root.sock";

const STATUS_MARKER: &str = "__agentic_job_test_status=";

const MODE_VAR: &str = "AGENTIC_JOB_WORLD_WRITE";

/// Where root makes the files the cases write.
const DIR: &str = "/var/lib/agentic-job-world-write";

/// Root's world-writable file made before setup, which the walk chmods and
/// the program denies.
const BEFORE_SETUP: &str = "/var/lib/agentic-job-world-write-before-setup";

const PIN_DIR: &str = "/sys/fs/bpf/agentic-job";

/// Root's files for the cases, made after setup: what the walk cannot
/// have seen.
const FIXTURES: &str = r#"
set -e
mkdir -p DIR && cd DIR
echo x > after-setup-666 && chmod 666 after-setup-666
echo x > group-664 && chgrp GID group-664 && chmod 664 group-664
echo x > group-666 && chgrp GID group-666 && chmod 666 group-666
mkdir -p dir-777 && chmod 777 dir-777
mkdir -p sticky-1777 && chmod 1777 sticky-1777
touch dir-777/victim && chmod 666 dir-777/victim
"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expect {
    /// Writable in both modes.
    Allowed,
    /// Denied in both modes.
    Denied,
    /// Denied by the program; the walk never saw it, so it stays open.
    DeniedByLsmOnly,
}

/// What the runner's user does, as a shell script with `$1` set to DIR.
struct Case {
    name: &'static str,
    script: &'static str,
    expect: Expect,
}

const CASES: &[Case] = &[
    Case {
        name: "a world-writable file of root's made before setup",
        script: "echo probe >> /var/lib/agentic-job-world-write-before-setup",
        expect: Expect::Denied,
    },
    Case {
        name: "a world-writable file of root's made after setup",
        script: r#"echo probe >> "$1/after-setup-666""#,
        expect: Expect::DeniedByLsmOnly,
    },
    Case {
        name: "creating in a world-writable directory of root's made after setup",
        script: r#"echo probe > "$1/dir-777/new""#,
        expect: Expect::DeniedByLsmOnly,
    },
    Case {
        name: "a hard link to a world-writable file of root's",
        script: r#"ln "$1/dir-777/victim" "$HOME/link-to-victim""#,
        expect: Expect::DeniedByLsmOnly,
    },
    Case {
        name: "a file root owns that the user's group may write",
        script: r#"echo probe >> "$1/group-664""#,
        expect: Expect::Allowed,
    },
    Case {
        name: "a world-writable file root owns that the user's group may write too",
        script: r#"echo probe >> "$1/group-666""#,
        expect: Expect::Allowed,
    },
    Case {
        name: "creating in a sticky world-writable directory of root's",
        script: r#"echo probe > "$1/sticky-1777/$$" && rm "$1/sticky-1777/$$""#,
        expect: Expect::Allowed,
    },
    Case {
        name: "creating in /tmp and /dev/shm",
        script: "echo probe > /tmp/world-write-$$ && rm /tmp/world-write-$$ && echo probe > /dev/shm/world-write-$$ && rm /dev/shm/world-write-$$",
        expect: Expect::Allowed,
    },
    Case {
        name: "the user's own world-writable file",
        script: r#"f="$HOME/own-666" && echo x > "$f" && chmod 666 "$f" && echo y >> "$f" && rm "$f""#,
        expect: Expect::Allowed,
    },
    Case {
        name: "/dev/null and the terminal devices",
        script: "echo probe > /dev/null && echo probe > /dev/zero && test -w /dev/ptmx",
        expect: Expect::Allowed,
    },
    Case {
        name: "the system bus, a Unix socket of root's",
        script: "busctl --system --no-pager list > /dev/null",
        expect: Expect::Allowed,
    },
    Case {
        name: "procfs: the process's own files",
        script: "echo 0 > /proc/self/oom_score_adj",
        expect: Expect::Allowed,
    },
    Case {
        name: "a rootless container: root inside makes a directory anyone may write, and another uid writes it",
        script: r#"podman run --rm docker.io/library/alpine sh -c 'mkdir /app && chmod 777 /app && su -s /bin/sh nobody -c "echo hi > /app/f && cat /app/f"'"#,
        expect: Expect::Allowed,
    },
    Case {
        name: "a rootless container writing its own layers and a volume of the user's",
        script: r#"mkdir -p "$HOME/vol" && podman run --rm -v "$HOME/vol:/vol" docker.io/library/alpine sh -c 'echo hi > /vol/f && echo hi > /etc/f && cat /vol/f' && rm -rf "$HOME/vol""#,
        expect: Expect::Allowed,
    },
];

fn mode() -> String {
    std::env::var(MODE_VAR).unwrap_or_else(|_| panic!("{MODE_VAR} must be walk or lsm"))
}

/// Runs `script` as root through the socket, with everything it printed.
fn root_sh(script: &str) -> (bool, String) {
    let mut socket = UnixStream::connect(ROOT_SOCKET)
        .unwrap_or_else(|err| panic!("connecting to {ROOT_SOCKET}: {err}"));
    let quoted = format!("'{}'", script.replace('\'', "'\\''"));
    let wrapped = format!("sh -c {quoted} 2>&1\nprintf '\\n{STATUS_MARKER}%s\\n' \"$?\"\nexit\n");
    socket.write_all(wrapped.as_bytes()).unwrap();
    socket.shutdown(Shutdown::Write).unwrap();
    let mut output = String::new();
    socket.read_to_string(&mut output).unwrap();
    let (printed, status) = output
        .rsplit_once(STATUS_MARKER)
        .unwrap_or_else(|| panic!("the root service sent no status: {output}"));
    (status.trim() == "0", printed.trim_end().to_owned())
}

/// Runs `script` as this user, with `$1` set to DIR.
fn user_sh(script: &str) -> (bool, String) {
    let out = Command::new("sh")
        .args(["-c", script, "sh", DIR])
        .output()
        .unwrap();
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
        .trim_end()
        .to_owned(),
    )
}

#[test]
#[ignore = "changes the host; CI's spike job runs it after sandbox setup"]
fn writes_as_a_non_root_user() {
    let mode = mode();
    let lsm = match mode.as_str() {
        "lsm" => true,
        "walk" => false,
        other => panic!("{MODE_VAR}={other}, not walk or lsm"),
    };
    let gid = Command::new("id").arg("-g").output().unwrap();
    let gid = String::from_utf8(gid.stdout).unwrap().trim().to_owned();
    let (ok, output) = root_sh(&FIXTURES.replace("DIR", DIR).replace("GID", &gid));
    assert!(ok, "making the fixtures as root: {output}");
    let before = std::fs::metadata(BEFORE_SETUP)
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    // What the walk did to the file made before setup (o-w), and the
    // program did not need to.
    assert_eq!(
        before,
        if lsm { 0o666 } else { 0o664 },
        "{BEFORE_SETUP}'s mode"
    );

    let mut failures = Vec::new();
    for case in CASES {
        let (allowed, output) = user_sh(case.script);
        let want = match case.expect {
            Expect::Allowed => true,
            Expect::Denied => false,
            Expect::DeniedByLsmOnly => !lsm,
        };
        let verdict = if allowed == want { "ok  " } else { "FAIL" };
        println!(
            "{verdict} [{mode}] {}: {}\n      {}",
            case.name,
            if allowed { "allowed" } else { "denied" },
            output.replace('\n', "\n      ")
        );
        if allowed != want {
            failures.push(case.name);
        }
    }
    assert!(
        failures.is_empty(),
        "cases that went the wrong way: {failures:?}"
    );

    if lsm {
        // The user cannot see or remove the pins that hold the program.
        let (listed, output) = user_sh(&format!("ls {PIN_DIR}"));
        assert!(!listed, "the user listed {PIN_DIR}: {output}");
        let (removed, output) = user_sh(&format!("rm -rf {PIN_DIR}"));
        assert!(!removed, "the user removed {PIN_DIR}: {output}");
        // Root removing the pin directory detaches the program after a
        // grace period: the same write is then allowed, so the denial was
        // the program's and nothing else's.
        let (ok, output) = root_sh(&format!("rm -rf {PIN_DIR}"));
        assert!(ok, "removing the pins as root: {output}");
        std::thread::sleep(std::time::Duration::from_secs(3));
        let (allowed, output) = user_sh(r#"echo probe >> "$1/after-setup-666""#);
        assert!(
            allowed,
            "after the pins were removed, the write was still denied: {output}"
        );
        println!(
            "ok   [{mode}] with the program detached, the world-writable file is writable again"
        );
    }
}
