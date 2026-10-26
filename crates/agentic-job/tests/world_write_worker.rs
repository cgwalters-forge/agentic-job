//! The world-write host stages a copy of `world-write-evidence` outside the
//! checkout, whose ancestors the worker user need not be able to search, and
//! runs its `worker` mode there. Prove such a copy needs nothing from the
//! checkout: run it from a temporary directory against an owner-owned fixture.
//! This is an integration test because only these get the built executable.
#![allow(clippy::unwrap_used)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

fn set_mode(path: &Path, mode: u32) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

#[test]
fn staged_worker_runs_without_anything_from_the_checkout() {
    let root = tempfile::tempdir().unwrap();
    let staged = root.path().join("world-write-evidence");
    std::fs::copy(env!("CARGO_BIN_EXE_world-write-evidence"), &staged).unwrap();
    set_mode(&staged, 0o755);
    // The fixture `fixtures(path, false, true, own uid)` builds.
    let fixture = root.path().join("fixture");
    let target = root.path().join("fixture-target");
    for dir in [&fixture, &target] {
        std::fs::create_dir(dir).unwrap();
    }
    for name in ["file", "unlink", "rename"] {
        let file = fixture.join(name);
        std::fs::write(&file, "fixture").unwrap();
        set_mode(&file, 0o666);
    }
    std::fs::write(target.join("source"), "fixture").unwrap();
    for dir in [&fixture, &target] {
        set_mode(dir, 0o777);
    }
    let output = Command::new(&staged)
        .args(["worker", fixture.to_str().unwrap(), "false"])
        .current_dir(root.path())
        .env_clear()
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if rustix::process::getuid().is_root() {
        // The worker refuses root, as the hosted one runs as nobody.
        assert!(!output.status.success(), "{stdout}{stderr}");
        return;
    }
    assert!(output.status.success(), "{stdout}{stderr}");
    assert!(stdout.contains("rename-destination allowed"), "{stdout}");
}
