//! Validate only explicitly named image binaries, never discover privileged files.
use std::{fs, io::ErrorKind, os::unix::fs::MetadataExt, path::Path};

use anyhow::{Context, Result, bail, ensure};

const ALLOWLIST: &str = include_str!("privileged-binaries.tsv");

/// Package-created groups of setgid programs: their ids are allocated per image.
const GROUP_DB: &str = "/etc/group";

#[derive(Debug)]
struct Entry<'a> {
    path: &'a str,
    uid: u32,
    gid: Gid<'a>,
    mode: u32,
}

#[derive(Debug, PartialEq)]
enum Gid<'a> {
    Id(u32),
    Name(&'a str),
}

fn entries<'a>(data: &'a str, id: &str, version: &str) -> Result<Vec<Entry<'a>>> {
    let mut entries = Vec::new();
    for (index, line) in data.lines().enumerate() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let fields: Vec<_> = line.split_whitespace().collect();
        ensure!(
            fields.len() == 6,
            "invalid privileged binary allowlist line {}",
            index + 1
        );
        if fields[0] != id || fields[1] != version {
            continue;
        }
        ensure!(
            Path::new(fields[2]).is_absolute(),
            "allowlist path must be absolute"
        );
        let gid = match fields[4].parse() {
            Ok(gid) => Gid::Id(gid),
            Err(_)
                if fields[4]
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_') =>
            {
                Gid::Name(fields[4])
            }
            Err(_) => bail!("invalid allowlist group {:?}", fields[4]),
        };
        entries.push(Entry {
            path: fields[2],
            uid: fields[3].parse().context("parsing allowlist uid")?,
            gid,
            mode: u32::from_str_radix(fields[5], 8).context("parsing allowlist mode")?,
        });
    }
    ensure!(
        !entries.is_empty(),
        "no privileged binary allowlist for {id} {version}; supported images are Ubuntu 26.04 and RHEL 10"
    );
    Ok(entries)
}

/// The id of a local group, from `/etc/group` text.
fn group_id(groups: &str, name: &str) -> Result<u32> {
    groups
        .lines()
        .find_map(|line| {
            let mut fields = line.split(':');
            (fields.next() == Some(name)).then(|| fields.nth(1))?
        })
        .with_context(|| format!("group {name} of a privileged binary is not in {GROUP_DB}"))?
        .parse()
        .with_context(|| format!("parsing the id of group {name}"))
}

fn validate(entry: &Entry<'_>, gid: u32, metadata: &fs::Metadata) -> Result<()> {
    ensure!(
        metadata.is_file()
            && metadata.uid() == entry.uid
            && metadata.gid() == gid
            && metadata.mode() & 0o7777 == entry.mode,
        "privileged binary {} differs from allowlist: expected regular file owner {}:{} mode {:04o}, got owner {}:{} mode {:04o}; use a supported fresh image",
        entry.path,
        entry.uid,
        gid,
        entry.mode,
        metadata.uid(),
        metadata.gid(),
        metadata.mode() & 0o7777
    );
    Ok(())
}

/// Whether the entry's binary is on this image; absent ones are not an error.
fn check_entry(entry: &Entry<'_>, groups: &str) -> Result<bool> {
    // Follow links like stat(1): Ubuntu's /usr/bin/sudo is an alternatives
    // link to sudo-rs, and what runs is the target's owner and mode.
    let metadata = match fs::metadata(entry.path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(false),
        Err(err) => {
            return Err(err)
                .with_context(|| format!("stat known privileged binary {}", entry.path));
        }
    };
    let gid = match entry.gid {
        Gid::Id(gid) => gid,
        Gid::Name(name) => group_id(groups, name)?,
    };
    validate(entry, gid, &metadata)?;
    Ok(true)
}

pub(super) fn check() -> Result<()> {
    let release = fs::read_to_string("/etc/os-release").context("reading image identity")?;
    let field = |name: &str| {
        release.lines().find_map(|line| {
            let (key, value) = line.split_once('=')?;
            (key == name).then(|| value.trim_matches('"'))
        })
    };
    let id = field("ID").context("os-release lacks ID")?;
    let version = field("VERSION_ID").context("os-release lacks VERSION_ID")?;
    let version = if id == "rhel" {
        version.split('.').next().context("empty RHEL version")?
    } else {
        version
    };
    let entries = entries(ALLOWLIST, id, version)?;
    let groups = fs::read_to_string(GROUP_DB).with_context(|| format!("reading {GROUP_DB}"))?;
    let mut present = 0;
    for entry in &entries {
        present += usize::from(check_entry(entry, &groups)?);
    }
    println!(
        "Checked {present} known privileged binaries for {id} {version}, {} listed ones absent (no traversal)",
        entries.len() - present
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn supported_images_and_invalid_data() {
        for (id, version, accepted) in [
            ("ubuntu", "26.04", true),
            ("rhel", "10", true),
            ("ubuntu", "24.04", false),
            ("rhel", "9", false),
        ] {
            assert_eq!(entries(ALLOWLIST, id, version).is_ok(), accepted);
        }
        for data in [
            "rhel 10 /bin/su 0",
            "rhel 10 relative 0 0 4755",
            "rhel 10 /bin/su bad 0 4755",
            "rhel 10 /bin/su 0 0 9999",
            "rhel 10 /bin/su 0 a:b 4755",
        ] {
            assert!(entries(data, "rhel", "10").is_err(), "{data}");
        }
        let named = entries("rhel 10 /usr/bin/write 0 tty 2755", "rhel", "10").unwrap();
        assert_eq!(named[0].gid, Gid::Name("tty"));
    }

    #[test]
    fn group_names_resolve_locally() {
        let groups = "root:x:0:\ntty:x:5:\nbad:x:nan:\n";
        for (name, expected) in [
            ("tty", Some(5)),
            ("root", Some(0)),
            ("bad", None),
            ("ttyx", None),
        ] {
            assert_eq!(group_id(groups, name).ok(), expected, "{name}");
        }
    }

    #[test]
    fn metadata_mismatches_fail_closed() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("binary");
        fs::write(&path, b"fixture").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o4755)).unwrap();
        let metadata = fs::symlink_metadata(&path).unwrap();
        let entry = |mode| Entry {
            path: "/fixture",
            uid: metadata.uid(),
            gid: Gid::Id(metadata.gid()),
            mode,
        };
        for (uid, gid, mode, accepted) in [
            (metadata.uid(), metadata.gid(), 0o4755, true),
            (metadata.uid() + 1, metadata.gid(), 0o4755, false),
            (metadata.uid(), metadata.gid() + 1, 0o4755, false),
            (metadata.uid(), metadata.gid(), 0o2755, false),
        ] {
            let entry = Entry { uid, ..entry(mode) };
            assert_eq!(validate(&entry, gid, &metadata).is_ok(), accepted);
        }
        let directory = fs::metadata(temp.path()).unwrap();
        assert!(validate(&entry(0o4755), metadata.gid(), &directory).is_err());

        let groups = format!("fixture:x:{}:\n", metadata.gid());
        let link = temp.path().join("link");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        let missing = temp.path().join("missing");
        let dangling = temp.path().join("dangling");
        std::os::unix::fs::symlink(&missing, &dangling).unwrap();
        // Present and matching, linked to a match, absent, or present but wrong.
        for (path, gid, mode, result) in [
            (&path, Gid::Id(metadata.gid()), 0o4755, Some(true)),
            (&path, Gid::Name("fixture"), 0o4755, Some(true)),
            (&link, Gid::Id(metadata.gid()), 0o4755, Some(true)),
            (&missing, Gid::Id(metadata.gid()), 0o4755, Some(false)),
            (&dangling, Gid::Id(metadata.gid()), 0o4755, Some(false)),
            (&path, Gid::Id(metadata.gid()), 0o4711, None),
            (&link, Gid::Id(metadata.gid()), 0o4711, None),
            (&path, Gid::Name("unknown"), 0o4755, None),
        ] {
            let entry = Entry {
                path: path.to_str().unwrap(),
                gid,
                ..entry(mode)
            };
            assert_eq!(
                check_entry(&entry, &groups).ok(),
                result,
                "{}",
                path.display()
            );
        }
    }
}
