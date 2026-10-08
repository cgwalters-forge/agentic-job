//! Close world write with the external CO-RE object and bpftool.
//! Setup proves enforcement with a write, not a denial log.

use std::fs;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use anyhow::{Context, Result, bail, ensure};
use clap::{Subcommand, ValueEnum};

use super::host;
use crate::exit::Exit;

const PIN_DIR: &str = "/sys/fs/bpf/agentic-job";
const PIN_LINKS: &str = "/sys/fs/bpf/agentic-job/links";
const LSM_LIST: &str = "/sys/kernel/security/lsm";
pub const CANARY: &str = "/var/lib/agentic-job-world-writable-canary";
pub const MODE_FILE: &str = "/etc/agentic-job/world-write";
const CANARY_TEXT: &[u8] = b"A root-owned world-writable file for enforcement probes.\n";
const MODE_WORLD_WRITABLE: u32 = 0o666;
const MODE_ROOTS_ALONE: u32 = 0o700;
const MODE_READABLE: u32 = 0o644;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Mode {
    /// Take the other-write bit off every path on the root filesystem
    Walk,
    /// Attach the BPF program that denies such writes instead
    Lsm,
}

impl Mode {
    fn name(self) -> &'static str {
        match self {
            Self::Walk => "walk",
            Self::Lsm => "lsm",
        }
    }

    /// Older setups without a mode file used the walk.
    pub fn on_host() -> Result<Self> {
        match fs::read_to_string(MODE_FILE) {
            Ok(text) if text.trim() == Self::Lsm.name() => Ok(Self::Lsm),
            Ok(text) if text.trim() == Self::Walk.name() => Ok(Self::Walk),
            Ok(text) => bail!("{MODE_FILE} says {text:?}, which is neither walk nor lsm"),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Self::Walk),
            Err(err) => Err(err).with_context(|| format!("reading {MODE_FILE}")),
        }
    }

    pub fn record(self) -> Result<()> {
        fs::write(MODE_FILE, format!("{}\n", self.name()))
            .with_context(|| format!("writing {MODE_FILE}"))?;
        fs::set_permissions(MODE_FILE, fs::Permissions::from_mode(MODE_READABLE))
            .with_context(|| format!("setting the mode of {MODE_FILE}"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Hook {
    /// The LSM hook where bpf is in the kernel's LSM list, else fmod_ret
    Auto,
    /// Require the BPF LSM hook to be active
    Lsm,
    /// Require modify-return on security_inode_permission()
    FmodRet,
}

impl Hook {
    fn resolve(self) -> Result<Self> {
        let active = bpf_lsm_active()?;
        match self {
            Self::Auto => Ok(if active { Self::Lsm } else { Self::FmodRet }),
            Self::Lsm => {
                ensure!(active, "bpf is not active in {LSM_LIST}");
                Ok(self)
            }
            Self::FmodRet => Ok(self),
        }
    }

    fn describe(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Lsm => "lsm/inode_permission",
            Self::FmodRet => "fmod_ret/security_inode_permission",
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum Op {
    /// Load, attach and pin the object, and prove it denies as USER
    Load {
        #[arg(long, value_name = "FILE")]
        object: PathBuf,
        #[arg(long, value_enum, default_value_t = Hook::Auto)]
        hook: Hook,
        #[arg(long, value_name = "USER")]
        test_as: String,
    },
    /// Print pin presence and the kernel's active LSMs
    Status,
}

impl Op {
    pub fn run(&self) -> Result<Exit> {
        ensure!(host::is_root(), "`sandbox world-write` runs as root");
        match self {
            Self::Load {
                object,
                hook,
                test_as,
            } => println!("{}", load(object, *hook, test_as)?),
            Self::Status => println!(
                "pins present: {}\nbpf in {LSM_LIST}: {}\nmode left by setup: {}",
                attached(),
                bpf_lsm_active()?,
                Mode::on_host()?.name()
            ),
        }
        Ok(Exit::Success)
    }
}

#[derive(Debug)]
pub struct Loaded {
    pub hook: Hook,
    pub seconds: f64,
}

impl std::fmt::Display for Loaded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "World write closed by {}, loaded and pinned by bpftool under {PIN_DIR}, proved to deny, in {:.1} s",
            self.hook.describe(),
            self.seconds
        )
    }
}

pub fn bpf_lsm_active() -> Result<bool> {
    let list = fs::read_to_string(LSM_LIST).with_context(|| format!("reading {LSM_LIST}"))?;
    Ok(list.trim().split(',').any(|lsm| lsm == "bpf"))
}

/// The object contains both entry points. Use loadall so libbpf attaches
/// both rather than silently choosing the first program. The resolved hook
/// describes the host's enforcement mechanism, not an ELF program selector.
/// Any error aborts setup; partial attachments are cleaned up as well.
pub fn load(object: &Path, hook: Hook, test_as: &str) -> Result<Loaded> {
    let started = Instant::now();
    let hook = hook.resolve()?;
    let uid = host::run(Command::new("id").args(["-u", "--", test_as]))?;
    ensure!(
        uid.trim()
            .parse::<u32>()
            .context("parsing test user's uid")?
            != 0,
        "the denial must be proved as a non-root user"
    );
    ensure!(
        object.is_file(),
        "BPF object {} is not a file",
        object.display()
    );
    ensure!(
        !Path::new(PIN_DIR).exists(),
        "{PIN_DIR} already exists; refusing to replace a running policy"
    );
    make_canary()?;
    ensure!(
        canary_write(test_as)?,
        "{test_as} cannot write {CANARY} before attachment: the control failed"
    );
    fs::DirBuilder::new()
        .mode(MODE_ROOTS_ALONE)
        .create(PIN_DIR)
        .with_context(|| format!("creating {PIN_DIR} (is bpffs mounted?)"))?;
    let result = (|| -> Result<()> {
        host::run(
            Command::new("bpftool")
                .args(["prog", "loadall"])
                .arg(object)
                .args([PIN_LINKS, "autoattach"]),
        )
        .context("bpftool prog loadall autoattach (requires bpftool and kernel BTF)")?;
        ensure!(
            attached(),
            "bpftool did not leave the expected attachment pins"
        );
        ensure!(
            !canary_write(test_as)?,
            "{} attached but {test_as} still wrote {CANARY}: refusing to finish setup",
            hook.describe()
        );
        Ok(())
    })();
    if let Err(err) = result {
        fs::remove_dir_all(PIN_DIR)
            .context(format!("{err:#}; removing partial BPF attachments"))?;
        return Err(err);
    }
    Ok(Loaded {
        hook,
        seconds: started.elapsed().as_secs_f64(),
    })
}

fn make_canary() -> Result<()> {
    match fs::remove_file(CANARY) {
        Ok(()) => (),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => (),
        Err(err) => return Err(err).context("removing the old canary"),
    }
    fs::write(CANARY, CANARY_TEXT).with_context(|| format!("creating {CANARY}"))?;
    fs::set_permissions(CANARY, fs::Permissions::from_mode(MODE_WORLD_WRITABLE))
        .with_context(|| format!("making {CANARY} world-writable"))
}

fn canary_write(user: &str) -> Result<bool> {
    let output = Command::new("runuser")
        .args([
            "-u",
            user,
            "--",
            "sh",
            "-c",
            r#"if (echo probe >> "$1"); then exit 0; else echo write-refused; exit 3; fi"#,
            "sh",
            CANARY,
        ])
        .output()
        .context("running the canary write probe")?;
    probe_result(output.status.code(), &output.stdout, &output.stderr)
}

fn probe_result(code: Option<i32>, stdout: &[u8], stderr: &[u8]) -> Result<bool> {
    match code {
        Some(0) => Ok(true),
        Some(3) if stdout == b"write-refused\n" => Ok(false),
        _ => bail!(
            "canary probe did not complete normally (exit {code:?}): {}",
            String::from_utf8_lossy(stderr)
        ),
    }
}

/// Pin presence alone is not proof of enforcement; sandbox check writes.
pub fn attached() -> bool {
    ["lsm_inode_permission", "fmod_ret_inode_permission"]
        .iter()
        .all(|name| Path::new(PIN_LINKS).join(name).exists())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_requires_the_write_to_have_run() {
        assert!(probe_result(Some(0), b"", b"").unwrap());
        assert!(!probe_result(Some(3), b"write-refused\n", b"Permission denied").unwrap());
        for code in [None, Some(1), Some(2), Some(3), Some(127)] {
            assert!(probe_result(code, b"", b"runuser failed").is_err());
        }
    }
}
