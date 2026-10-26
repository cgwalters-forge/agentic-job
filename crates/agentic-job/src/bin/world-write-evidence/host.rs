//! Root's side of the proof. Fresh hosted runner only. Separate invocations
//! (`host before`, then `host after` once the policy is loaded) prove the
//! links outlive loading.

use crate::cases::{self, ALL_DENIED, NONE_DENIED};
use crate::command::run;
use anyhow::{Context, Result, bail, ensure};
use std::fs::DirBuilder;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(120);
const DIRECTORY: &str = "/world-write-ci";
/// The staged copy of this executable that the worker runs.
const STAGED_NAME: &str = "world-write-evidence";
const STAGED_MODE: u32 = 0o755;
const WORKER: &str = "nobody";

/// A row of the operation matrix.
struct Case {
    path: String,
    /// The worker's DENIED argument once the policy is active.
    denies: &'static str,
    sticky: bool,
    owner: bool,
}

/// Runs a command with inherited output, failing unless it succeeds in time.
fn exec(program: &str, args: &[&str]) -> Result<()> {
    run(program, args, false, TIMEOUT).map(drop)
}

fn dev(path: &str) -> Result<u64> {
    let meta = std::fs::metadata(path).with_context(|| format!("stat {path}"))?;
    Ok(meta.dev())
}

fn set_mode(path: &str, mode: u32) -> Result<()> {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .with_context(|| format!("changing the mode of {path} to {mode:o}"))
}

fn mkdir(path: &str, mode: u32, recursive: bool) -> Result<()> {
    DirBuilder::new()
        .mode(mode)
        .recursive(recursive)
        .create(path)
        .with_context(|| format!("creating {path}"))
}

fn remove_tree(path: &str) -> Result<()> {
    std::fs::remove_dir_all(path).with_context(|| format!("removing {path}"))
}

fn worker_uid() -> Result<u32> {
    let out = run("id", &["-u", WORKER], true, TIMEOUT)?;
    let uid: u32 = out
        .trim()
        .parse()
        .with_context(|| format!("uid of {WORKER}: {out:?}"))?;
    ensure!(uid > 0, "{WORKER} is root");
    Ok(uid)
}

/// Runs `host before` or `host after` as root.
pub fn host(mode: &str) -> Result<()> {
    let directory = DIRECTORY;
    let staged = format!("{directory}/{STAGED_NAME}");
    let foreign = format!("{directory}/foreign");
    let owned = format!("{directory}/owned");
    let uid = worker_uid()?;
    let mount = format!("{directory}/tmpfs");
    // A container's root: an overlay whose layers are on the root filesystem,
    // mounted by root, as rootful Podman's storage is. Overlayfs creates the
    // upper entry with the caller's fsuid (its creator credentials), so the policy
    // refuses a create in a foreign world-writable upper parent; unlink and rename
    // run with the mounter's (root's) credentials and are allowed.
    let overlay = format!("{directory}/overlay");
    let layers = format!("{directory}/layers");
    let case = |path: String, denies, sticky, owner| Case {
        path,
        denies,
        sticky,
        owner,
    };
    let cases = [
        case(format!("{directory}/nonsticky"), ALL_DENIED, false, false),
        case(format!("{directory}/sticky"), NONE_DENIED, true, false),
        case(format!("{directory}/owner"), NONE_DENIED, false, true),
        case(format!("{mount}/foreign"), NONE_DENIED, false, false),
        case(format!("{overlay}/foreign"), "create", false, false),
    ];
    match mode {
        "before" => {
            mkdir(directory, 0o755, false)?;
            // Hosted checkout ancestors need not be searchable by nobody. Keep the
            // worker in a root-owned, searchable location instead: this executable
            // carries the shared operations, so nothing else is staged.
            let exe = std::env::current_exe().context("locating this executable")?;
            std::fs::copy(&exe, &staged)
                .with_context(|| format!("copying {} to {staged}", exe.display()))?;
            set_mode(&staged, STAGED_MODE)?;
            ensure!(dev(directory)? == dev("/")?, "{directory} is not on /");
            for path in [&foreign, &owned] {
                std::fs::write(path, "fixture").with_context(|| format!("writing {path}"))?;
                set_mode(path, 0o666)?;
            }
            rustix::fs::chown(owned.as_str(), Some(rustix::fs::Uid::from_raw(uid)), None)
                .with_context(|| format!("changing the owner of {owned}"))?;
            mkdir(&mount, 0o777, false)?;
            let tmpfs = ["-t", "tmpfs", "-o", "size=1m", "tmpfs", &mount];
            exec("mount", &tmpfs)?;
            ensure!(dev(&mount)? != dev("/")?, "{mount} is not a separate mount");
            for layer in ["lower", "upper", "work"] {
                mkdir(&format!("{layers}/{layer}"), 0o755, true)?;
            }
            mkdir(&overlay, 0o777, false)?;
            let options =
                format!("lowerdir={layers}/lower,upperdir={layers}/upper,workdir={layers}/work");
            let overlayfs = ["-t", "overlay", "-o", &options, "overlay", &overlay];
            exec("mount", &overlayfs)?;
            ensure!(
                dev(&overlay)? != dev("/")?,
                "{overlay} is not a separate mount"
            );
            ensure!(
                dev(&format!("{layers}/upper"))? == dev("/")?,
                "{layers}/upper is not on /"
            );
        }
        "after" => {}
        other => bail!("host mode must be before or after, not {other:?}"),
    }
    let denied = mode == "after";
    for (path, expected) in [(&foreign, denied), (&owned, false)] {
        let expected = if expected { ALL_DENIED } else { NONE_DENIED };
        let append = ["-u", WORKER, "--", &staged, "append", path, expected];
        exec("runuser", &append)?;
    }
    for case in &cases {
        let path = case.path.as_str();
        if !path.starts_with(&mount) {
            ensure!(dev(directory)? == dev("/")?, "{directory} is not on /");
        }
        let worker_target = !path.starts_with(&overlay);
        let plan = cases::fixtures(path, case.sticky, case.owner, uid, worker_target);
        cases::apply(&plan)?;
        let expected = if denied { case.denies } else { NONE_DENIED };
        let worker = ["-u", WORKER, "--", &staged, "worker", path, expected];
        exec("runuser", &worker)?;
        // Root has no policy exemption either. Close worker-owned parents before
        // privileged fixture cleanup, rather than accidentally testing a root bypass.
        let target = format!("{path}-target");
        set_mode(path, 0o755)?;
        set_mode(&target, 0o755)?;
        remove_tree(path)?;
        remove_tree(&target)?;
    }
    if denied {
        exec("umount", &[&mount])?;
        exec("umount", &[&overlay])?;
        remove_tree(directory)?;
    }
    println!(
        "world-write {mode}: foreign write {}; owner write allowed",
        if denied { "denied" } else { "allowed" }
    );
    Ok(())
}

/// `append PATH true|false`: the owner-write check, run as the worker.
pub fn append(path: &str, expected: &str) -> Result<()> {
    match expected {
        ALL_DENIED => cases::expect_denied(path, cases::append(path)),
        NONE_DENIED => cases::append(path).with_context(|| format!("appending to {path}")),
        other => bail!("expected must be true or false, not {other:?}"),
    }
}

/// `worker DIR DENIED`: the operation matrix, never as root.
pub fn worker(directory: &str, denied: &str) -> Result<()> {
    ensure!(
        !rustix::process::getuid().is_root(),
        "the worker must not run as root"
    );
    cases::exercise(directory, &cases::parse_denied(denied))
}
