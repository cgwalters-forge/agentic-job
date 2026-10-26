//! Shared by the unprivileged hosted worker and ordinary regression tests.

use anyhow::{Context, Result, anyhow, bail, ensure};
use std::fs::{DirBuilder, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

const FIXTURE: &[u8] = b"fixture";
const FIXTURE_FILES: &[&str] = &["file", "unlink", "rename"];
const TARGET_SUFFIX: &str = "-target";
const PARENT_MODE: u32 = 0o755;
const FILE_MODE: u32 = 0o666;
const OPEN_MODE: u32 = 0o777;
const STICKY_MODE: u32 = 0o1777;

/// The worker's argument naming every operation as denied, or none.
pub const ALL_DENIED: &str = "true";
pub const NONE_DENIED: &str = "false";

/// The operations in the order they run, by the names `exercise` accepts.
pub const OPERATIONS: &[&str] = &[
    "write",
    "create",
    "unlink",
    "rename-source",
    "rename-destination",
];

/// One step of building a fixture, as data so tests can check the order.
#[derive(Debug)]
pub enum FixtureOp {
    Mkdir {
        path: String,
        mode: u32,
    },
    /// Creates (or replaces) a file containing `fixture`.
    Write {
        path: String,
    },
    /// Changes only the owner; the group is kept.
    Chown {
        path: String,
        uid: u32,
    },
    Chmod {
        path: String,
        mode: u32,
    },
}

/// The steps that build `path` and its rename parent `path-target`.
///
/// worker_target: the rename parent `-target` belongs to the worker. Not in an
/// overlay, which checks its upper inodes again with the mounter's (root's)
/// credentials: that would test ownership in the upper layer, not the scope.
pub fn fixtures(
    path: &str,
    sticky: bool,
    owner: bool,
    uid: u32,
    worker_target: bool,
) -> Vec<FixtureOp> {
    let target = format!("{path}{TARGET_SUFFIX}");
    let directories = [path.to_owned(), target.clone()];
    let mut ops: Vec<FixtureOp> = directories
        .iter()
        .map(|dir| FixtureOp::Mkdir {
            path: dir.clone(),
            mode: PARENT_MODE,
        })
        .collect();
    for name in FIXTURE_FILES {
        let file = format!("{path}/{name}");
        ops.push(FixtureOp::Write { path: file.clone() });
        if owner || sticky {
            ops.push(FixtureOp::Chown {
                path: file.clone(),
                uid,
            });
        }
        ops.push(FixtureOp::Chmod {
            path: file,
            mode: FILE_MODE,
        });
    }
    let source = format!("{target}/source");
    ops.push(FixtureOp::Write {
        path: source.clone(),
    });
    ops.push(FixtureOp::Chown { path: source, uid });
    // Populate first: the policy has no root exemption for foreign writable parents.
    for dir in directories {
        // The target is owned by the worker: isolate each rename's tested parent.
        if owner || (worker_target && dir.ends_with(TARGET_SUFFIX)) {
            ops.push(FixtureOp::Chown {
                path: dir.clone(),
                uid,
            });
        }
        ops.push(FixtureOp::Chmod {
            path: dir,
            mode: if sticky { STICKY_MODE } else { OPEN_MODE },
        });
    }
    ops
}

/// Applies `fixtures`' steps to the real filesystem, in order.
pub fn apply(ops: &[FixtureOp]) -> Result<()> {
    for op in ops {
        match op {
            FixtureOp::Mkdir { path, mode } => DirBuilder::new()
                .mode(*mode)
                .create(path)
                .with_context(|| format!("creating {path}"))?,
            FixtureOp::Write { path } => {
                std::fs::write(path, FIXTURE).with_context(|| format!("writing {path}"))?
            }
            FixtureOp::Chown { path, uid } => {
                rustix::fs::chown(path.as_str(), Some(rustix::fs::Uid::from_raw(*uid)), None)
                    .with_context(|| format!("changing the owner of {path} to {uid}"))?
            }
            FixtureOp::Chmod { path, mode } => {
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(*mode))
                    .with_context(|| format!("changing the mode of {path} to {mode:o}"))?
            }
        }
    }
    Ok(())
}

/// The worker's DENIED argument: `true` or `false` for every operation, or
/// the comma-separated names of those denied.
pub fn parse_denied(denied: &str) -> Vec<String> {
    match denied {
        ALL_DENIED => OPERATIONS.iter().map(|&name| name.to_owned()).collect(),
        NONE_DENIED => Vec::new(),
        names => names.split(',').map(str::to_owned).collect(),
    }
}

/// Appends to `path`, creating it if missing, as Node's appendFileSync did.
pub fn append(path: &str) -> std::io::Result<()> {
    OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)?
        .write_all(b"write")
}

fn operation(directory: &str, name: &str) -> std::io::Result<()> {
    let target = format!("{directory}{TARGET_SUFFIX}");
    match name {
        "write" => append(&format!("{directory}/file")),
        "create" => std::fs::write(format!("{directory}/created"), "created"),
        "unlink" => std::fs::remove_file(format!("{directory}/unlink")),
        "rename-source" => {
            std::fs::rename(format!("{directory}/rename"), format!("{target}/moved"))
        }
        "rename-destination" => std::fs::rename(
            format!("{target}/source"),
            format!("{directory}/destination"),
        ),
        // Names are checked before any operation runs.
        _ => Err(std::io::Error::other(format!("unknown operation {name}"))),
    }
}

/// Checks that an operation failed with EACCES: anything else, such as a
/// missing fixture's ENOENT, is not a policy denial.
pub fn expect_denied(what: &str, result: std::io::Result<()>) -> Result<()> {
    match result {
        Ok(()) => bail!("{what}: expected EACCES, but it was allowed"),
        Err(e) if e.raw_os_error() == Some(rustix::io::Errno::ACCESS.raw_os_error()) => Ok(()),
        // Not an io::Error in the chain: callers can tell a failed check
        // from an allowed operation's own failure.
        Err(e) => Err(anyhow!("{what}: expected EACCES, got {e}")),
    }
}

/// Runs every operation in `directory`, requiring EACCES for each named in
/// `refused` and success for the rest.
pub fn exercise(directory: &str, refused: &[String]) -> Result<()> {
    let unknown: Vec<&str> = refused
        .iter()
        .map(String::as_str)
        .filter(|name| !OPERATIONS.contains(name))
        .collect();
    ensure!(
        unknown.is_empty(),
        "unknown operations: {}",
        unknown.join(",")
    );
    for &name in OPERATIONS {
        let expected = refused.iter().any(|r| r == name);
        let result = operation(directory, name);
        if expected {
            expect_denied(&format!("{directory}: {name}"), result)?;
        } else {
            result.with_context(|| format!("{directory}: {name}"))?;
        }
        println!(
            "{directory}: {name} {}",
            if expected { "denied" } else { "allowed" }
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::Path;

    #[derive(Clone, Copy)]
    struct Entry {
        uid: u32,
        mode: u32,
    }

    #[test]
    fn fixtures_populate_parents_before_exposing_them_to_the_no_root_exemption_policy() {
        for (sticky, owner, worker_target) in [
            (false, false, true),
            (true, false, true),
            (false, true, true),
            (false, false, false),
        ] {
            let mut entries: HashMap<String, Entry> = HashMap::new();
            for op in fixtures("/fixture", sticky, owner, 123, worker_target) {
                match op {
                    FixtureOp::Mkdir { path, mode } => {
                        entries.insert(path, Entry { uid: 0, mode });
                    }
                    FixtureOp::Write { path } => {
                        let parent = entries[&path[..path.rfind('/').unwrap()]];
                        // Foreign, world-writable and not sticky: refused even to root.
                        let open = parent.mode & 0o002 != 0 && parent.mode & 0o1000 == 0;
                        let allowed = parent.uid == 0 || !open;
                        assert!(allowed, "root fixture creation denied in {path}");
                        entries.insert(
                            path,
                            Entry {
                                uid: 0,
                                mode: 0o644,
                            },
                        );
                    }
                    FixtureOp::Chown { path, uid } => entries.get_mut(&path).unwrap().uid = uid,
                    FixtureOp::Chmod { path, mode } => entries.get_mut(&path).unwrap().mode = mode,
                }
            }
            for dir in ["/fixture", "/fixture-target"] {
                assert_eq!(entries[dir].mode, if sticky { 0o1777 } else { 0o777 });
                let worker = owner || (worker_target && dir.ends_with("-target"));
                assert_eq!(entries[dir].uid, if worker { 123 } else { 0 }, "{dir}");
            }
            for name in ["file", "unlink", "rename"] {
                let entry = entries[&format!("/fixture/{name}")];
                assert_eq!(entry.mode, 0o666);
                assert_eq!(entry.uid, if owner || sticky { 123 } else { 0 });
            }
            assert_eq!(entries["/fixture-target/source"].uid, 123);
        }
    }

    fn exists(path: &str) -> bool {
        Path::new(path).exists()
    }

    fn names(names: &[&str]) -> Vec<String> {
        names.iter().map(|&n| n.to_owned()).collect()
    }

    fn own_uid() -> u32 {
        rustix::process::getuid().as_raw()
    }

    /// A failed check, as opposed to an operation's own I/O error.
    fn is_check_failure(e: &anyhow::Error) -> bool {
        e.downcast_ref::<std::io::Error>().is_none()
    }

    #[test]
    fn allowed_matrix_exercises_both_rename_parents_and_entry_operations() {
        let root = tempfile::tempdir().unwrap();
        let path = format!("{}/fixture", root.path().display());
        std::fs::create_dir(&path).unwrap();
        std::fs::create_dir(format!("{path}-target")).unwrap();
        for name in ["file", "unlink", "rename"] {
            std::fs::write(format!("{path}/{name}"), "fixture").unwrap();
        }
        std::fs::write(format!("{path}-target/source"), "fixture").unwrap();
        exercise(&path, &parse_denied("false")).unwrap();
        for name in ["file", "created", "destination"] {
            assert!(exists(&format!("{path}/{name}")), "{name}");
        }
        for name in ["unlink", "rename"] {
            assert!(!exists(&format!("{path}/{name}")), "{name}");
        }
        assert!(exists(&format!("{path}-target/moved")));
        assert!(!exists(&format!("{path}-target/source")));
    }

    #[test]
    fn a_missing_fixture_is_not_accepted_as_a_policy_denial() {
        let root = tempfile::tempdir().unwrap();
        let absent = format!("{}/absent", root.path().display());
        let err = exercise(&absent, &parse_denied("true")).unwrap_err();
        assert!(is_check_failure(&err), "{err:#}");
        assert!(format!("{err:#}").contains("expected EACCES"), "{err:#}");
        let err = exercise(&absent, &parse_denied("false")).unwrap_err();
        let io = err.downcast_ref::<std::io::Error>().expect("an I/O error");
        assert_eq!(io.kind(), std::io::ErrorKind::NotFound, "{err:#}");
    }

    #[test]
    fn named_denials_must_each_be_refused_and_only_names_of_operations_are_accepted() {
        let root = tempfile::tempdir().unwrap();
        let path = format!("{}/fixture", root.path().display());
        apply(&fixtures(&path, false, true, own_uid(), true)).unwrap();
        let err = exercise(&path, &names(&["truncate"])).unwrap_err();
        assert!(is_check_failure(&err), "{err:#}");
        assert!(!exists(&format!("{path}/created")));
        // write is allowed and runs; the unexpectedly allowed create is not a denial.
        let err = exercise(&path, &names(&["create"])).unwrap_err();
        assert!(is_check_failure(&err), "{err:#}");
        assert!(err.to_string().contains("create"), "{err:#}");
        assert!(exists(&format!("{path}/created")));
    }

    #[test]
    fn parses_denied_arguments() {
        for (arg, want) in [
            ("true", OPERATIONS.to_vec()),
            ("false", vec![]),
            ("create", vec!["create"]),
            ("create,unlink", vec!["create", "unlink"]),
            ("", vec![""]),
        ] {
            assert_eq!(parse_denied(arg), names(&want), "{arg:?}");
        }
    }

    #[test]
    fn only_eacces_counts_as_a_denial() {
        use std::io::{Error, ErrorKind};
        let raw = |errno: rustix::io::Errno| Err(Error::from_raw_os_error(errno.raw_os_error()));
        assert!(expect_denied("x", raw(rustix::io::Errno::ACCESS)).is_ok());
        assert!(expect_denied("x", Ok(())).is_err());
        // EPERM is also PermissionDenied, but not what the policy returns.
        assert!(expect_denied("x", raw(rustix::io::Errno::PERM)).is_err());
        assert!(expect_denied("x", Err(Error::from(ErrorKind::NotFound))).is_err());
    }
}
