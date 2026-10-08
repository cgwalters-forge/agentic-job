//! World-writable paths, closed by a BPF program instead of the walk:
//! `agentic-job sandbox world-write`, which `sandbox setup --world-write
//! lsm` runs. A spike (docs/world-write.md): the program is built from
//! `bpf/world_write.bpf.c` outside the binary and given by path.
//!
//! The program denies, for a non-root uid, a write that Unix permissions
//! granted only through the other-write bit of a file or directory another
//! uid owns: what taking world write off every such path denies, with the
//! difference that it also holds for what is made world-writable after
//! setup. It attaches to the kernel's inode_permission check in one of two
//! ways, by what the kernel offers ([`Hook`]), and is proved to deny
//! before setup goes on ([`load`]): a program that attached and does not
//! deny (an LSM program on a kernel whose LSM list lacks bpf attaches, and
//! never runs) fails setup rather than leave the host open.

use std::fs;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use anyhow::{Context, Result, bail, ensure};
use aya::maps::{Map, MapData, RingBuf};
use aya::programs::links::FdLink;
use aya::programs::{FModRet, Lsm};
use aya::{Btf, Ebpf};
use clap::{Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};

use super::host;
use crate::exit::Exit;

/// Root's directory in the BPF filesystem that holds the pins; only root
/// can open what is in it, so only root can detach the program or read the
/// denials.
const PIN_DIR: &str = "/sys/fs/bpf/agentic-job";

/// The attachment, pinned: the program stays attached as long as this
/// exists.
const PIN_LINK: &str = "/sys/fs/bpf/agentic-job/world-write.link";

/// The ring buffer of denied writes, pinned for the helper to read.
const PIN_DENIALS: &str = "/sys/fs/bpf/agentic-job/denials";

/// The kernel's list of active LSMs.
const LSM_LIST: &str = "/sys/kernel/security/lsm";

/// A world-writable file of root's, left for the probes: writing to it as
/// any other user must fail while the program is attached. Not in
/// /etc/agentic-job, which setup wants absent on a fresh machine and
/// root's alone afterwards.
pub const CANARY: &str = "/var/lib/agentic-job-world-writable-canary";

/// What `sandbox setup` leaves to say how world write was closed:
/// `walk` or `lsm`; `sandbox check` picks its probe by it.
pub const MODE_FILE: &str = "/etc/agentic-job/world-write";

const CANARY_TEXT: &[u8] =
    b"A world-writable file of root's, kept to prove that writing to it is denied.\n";

const MODE_WORLD_WRITABLE: u32 = 0o666;

const MODE_ROOTS_ALONE: u32 = 0o700;

const MODE_READABLE: u32 = 0o644;

/// The programs in the object, by function name, and what each attaches to.
const LSM_PROGRAM: &str = "lsm_inode_permission";

const LSM_HOOK: &str = "inode_permission";

const FMOD_RET_PROGRAM: &str = "fmod_ret_inode_permission";

const FMOD_RET_FUNCTION: &str = "security_inode_permission";

const DENIALS_MAP: &str = "denials";

/// `struct denial` in the program: the reader refuses any other size.
const DENIAL_SIZE: usize = 48;

/// How world write is closed on this host.
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

    /// What setup left in [`MODE_FILE`]; the walk where it left nothing,
    /// since setups before this option walked.
    pub fn on_host() -> Result<Self> {
        match fs::read_to_string(MODE_FILE) {
            Ok(text) if text.trim() == Self::Lsm.name() => Ok(Self::Lsm),
            Ok(text) if text.trim() == Self::Walk.name() => Ok(Self::Walk),
            Ok(text) => bail!("{MODE_FILE} says {text:?}, which is neither walk nor lsm"),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Self::Walk),
            Err(err) => Err(err).with_context(|| format!("reading {MODE_FILE}")),
        }
    }

    /// Writes [`MODE_FILE`], as root.
    pub fn record(self) -> Result<()> {
        fs::write(MODE_FILE, format!("{}\n", self.name()))
            .with_context(|| format!("writing {MODE_FILE}"))?;
        fs::set_permissions(MODE_FILE, fs::Permissions::from_mode(MODE_READABLE))
            .with_context(|| format!("setting the mode of {MODE_FILE}"))
    }
}

/// How the program is attached.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Hook {
    /// The LSM hook where bpf is in the kernel's LSM list, else fmod_ret
    Auto,
    /// A BPF LSM program on the inode_permission hook
    Lsm,
    /// An fmod_ret program on security_inode_permission()
    FmodRet,
}

impl Hook {
    fn resolve(self) -> Result<Self> {
        Ok(match self {
            Self::Auto => {
                if bpf_lsm_active()? {
                    Self::Lsm
                } else {
                    Self::FmodRet
                }
            }
            chosen => chosen,
        })
    }

    /// The kernel attach type `bpftool link show` prints for this hook.
    fn attach_type(self) -> &'static str {
        match self {
            Self::Lsm => "lsm_mac",
            Self::FmodRet => "modify_return",
            Self::Auto => "",
        }
    }

    fn describe(self) -> String {
        match self {
            Self::Auto => "auto".to_owned(),
            Self::Lsm => format!("lsm/{LSM_HOOK} (a BPF LSM program)"),
            Self::FmodRet => format!("fmod_ret/{FMOD_RET_FUNCTION} (a modify-return program)"),
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum Op {
    /// Load the program, attach and pin it, and prove it denies as USER
    Load {
        /// The compiled program, bpf/world_write.bpf.o
        #[arg(long, value_name = "FILE")]
        object: PathBuf,
        #[arg(long, value_enum, default_value_t = Hook::Auto)]
        hook: Hook,
        /// A user other than root to prove the denial with
        #[arg(long, value_name = "USER")]
        test_as: String,
    },
    /// Print the denied writes recorded since last read, one JSON object a line
    Denials,
    /// Print whether the program is attached, and what the kernel offers
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
            } => {
                let loaded = load(object, *hook, test_as)?;
                println!("{loaded}");
            }
            Self::Denials => return print_denials(),
            Self::Status => {
                println!("{}", status()?);
            }
        }
        Ok(Exit::Success)
    }
}

/// Whether the attachment was pinned by aya or by bpftool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinVia {
    Aya,
    Bpftool,
}

impl PinVia {
    fn name(self) -> &'static str {
        match self {
            Self::Aya => "aya",
            Self::Bpftool => "bpftool (aya's pin is refused on this kernel)",
        }
    }
}

/// What loading did, for setup's output.
#[derive(Debug)]
pub struct Loaded {
    pub hook: Hook,
    pub via: PinVia,
    pub seconds: f64,
}

impl std::fmt::Display for Loaded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "World write closed by {}, pinned by {} under {PIN_DIR}, proved to deny, in {:.1} s",
            self.hook.describe(),
            self.via.name(),
            self.seconds
        )
    }
}

/// Whether `bpf` is among the kernel's active LSMs: only then does a BPF
/// LSM program run.
pub fn bpf_lsm_active() -> Result<bool> {
    let list = fs::read_to_string(LSM_LIST).with_context(|| format!("reading {LSM_LIST}"))?;
    Ok(list.trim().split(',').any(|lsm| lsm == "bpf"))
}

/// Loads the object, attaches its program by `hook`, pins the attachment
/// and the denials, and proves the denial as `test_as`: a write to the
/// canary that succeeds before and fails after. Fails closed: nothing is
/// left pinned when any of that fails.
pub fn load(object: &Path, hook: Hook, test_as: &str) -> Result<Loaded> {
    let started = Instant::now();
    ensure!(
        test_as != "root",
        "the denial is proved as a user other than root, which the program exempts"
    );
    let hook = hook.resolve()?;
    let bytes = fs::read(object).with_context(|| format!("reading {}", object.display()))?;
    make_canary()?;
    ensure!(
        canary_write(test_as)?,
        "{test_as} cannot write {CANARY} before the program is attached: the control failed"
    );
    let btf =
        Btf::from_sys_fs().context("reading the kernel's BTF from /sys/kernel/btf/vmlinux")?;
    unpin();
    fs::DirBuilder::new()
        .mode(MODE_ROOTS_ALONE)
        .recursive(true)
        .create(PIN_DIR)
        .with_context(|| format!("creating {PIN_DIR} (is the BPF filesystem mounted?)"))?;
    // aya first: it loads the object, applies the CO-RE relocations from
    // the running kernel's BTF, and attaches the program for the hook. If
    // its pin then fails (it does, on 6.12 and 7.0: see docs/world-write.md
    // and PinVia), bpftool pins the same object: aya proves the Rust path
    // for everything but the pin.
    let via = match aya_attach_and_pin(&bytes, hook, &btf) {
        Ok(()) => PinVia::Aya,
        // aya attaches but cannot pin on 6.12 or 7.0 (bpftool pins the same
        // link): fall back for the pin alone. A real attach failure shows
        // up as bpftool's error, not swallowed.
        Err(err) if is_aya_pin_failure(&err) => {
            println!(
                "aya could not pin ({}); pinning with bpftool instead",
                root_cause(&err)
            );
            unpin();
            pin_with_bpftool(object, hook)?;
            PinVia::Bpftool
        }
        Err(err) => {
            unpin();
            return Err(err);
        }
    };
    // The proof, with the loader's own references gone: a write the program
    // must deny.
    if canary_write(test_as)? {
        unpin();
        bail!(
            "{} attached, and {test_as} still wrote {CANARY}: the program does not run on this kernel{}",
            hook.describe(),
            if hook == Hook::Lsm && !bpf_lsm_active()? {
                format!(" (bpf is not in {LSM_LIST}: an LSM program attaches there and never runs)")
            } else {
                String::new()
            }
        );
    }
    Ok(Loaded {
        hook,
        via,
        seconds: started.elapsed().as_secs_f64(),
    })
}

/// Loads and attaches the program with aya, and pins the attachment and
/// its ring buffer. Returns once the loader's own references are dropped,
/// so a success means the pins hold the attachment by themselves.
fn aya_attach_and_pin(bytes: &[u8], hook: Hook, btf: &Btf) -> Result<()> {
    let mut ebpf =
        Ebpf::load(bytes).context("loading the object with aya (CO-RE relocation or map setup)")?;
    let link: FdLink = match hook {
        Hook::Lsm => {
            let program: &mut Lsm = ebpf
                .program_mut(LSM_PROGRAM)
                .with_context(|| format!("the object has no program {LSM_PROGRAM}"))?
                .try_into()
                .context("the program is not an LSM program")?;
            program
                .load(LSM_HOOK, btf)
                .context("loading the LSM program (the verifier's log follows)")?;
            let id = program.attach().context("attaching to the hook")?;
            program.take_link(id)?.into()
        }
        Hook::FmodRet => {
            let program: &mut FModRet = ebpf
                .program_mut(FMOD_RET_PROGRAM)
                .with_context(|| format!("the object has no program {FMOD_RET_PROGRAM}"))?
                .try_into()
                .context("the program is not an fmod_ret program")?;
            program
                .load(FMOD_RET_FUNCTION, btf)
                .context("loading the fmod_ret program (the verifier's log follows)")?;
            let id = program.attach().context("attaching to the function")?;
            program.take_link(id)?.into()
        }
        Hook::Auto => bail!("the hook was not resolved"),
    };
    link.pin(PIN_LINK)
        .with_context(|| format!("aya pinning the attachment at {PIN_LINK}"))?;
    ebpf.map_mut(DENIALS_MAP)
        .with_context(|| format!("the object has no map {DENIALS_MAP}"))?
        .pin(PIN_DENIALS)
        .with_context(|| format!("pinning the denials at {PIN_DENIALS}"))?;
    Ok(())
}

/// Loads, attaches and persists the program with bpftool, the workaround
/// for aya's pin. `loadall ... autoattach` loads both programs, attaches
/// each, and pins the programs under `progs` and the maps (so the ring
/// buffer lands at [`PIN_DENIALS`] by its name). The pinned programs hold
/// the attachment: it keeps denying after bpftool exits, and only
/// [`unpin`] (removing the whole directory) releases it. The link itself
/// is not pinned, because pinning a link is refused on 6.12 and 7.0 and
/// an LSM link cannot be detached, so the program pins are the handle
/// (docs/world-write.md). The hook's link is only looked up, to confirm
/// it attached.
fn pin_with_bpftool(object: &Path, hook: Hook) -> Result<()> {
    let object = object.to_str().context("the object's path is not UTF-8")?;
    fs::DirBuilder::new()
        .mode(MODE_ROOTS_ALONE)
        .recursive(true)
        .create(PIN_DIR)
        .with_context(|| format!("creating {PIN_DIR}"))?;
    let progs = format!("{PIN_DIR}/progs");
    host::run(Command::new("bpftool").args([
        "prog",
        "loadall",
        object,
        &progs,
        "autoattach",
        "pinmaps",
        PIN_DIR,
    ]))
    .context("bpftool prog loadall")?;
    bpftool_link_id(hook.attach_type())?;
    Ok(())
}

/// The id of the attached link whose kernel attach type is `attach_type`
/// (`lsm_mac` or `modify_return`), from `bpftool link show`.
fn bpftool_link_id(attach_type: &str) -> Result<u64> {
    let json = host::run(Command::new("bpftool").args(["-j", "link", "show"]))
        .context("bpftool link show")?;
    let links: Vec<serde_json::Value> =
        serde_json::from_str(&json).context("parsing bpftool link show")?;
    links
        .iter()
        .find(|link| link.get("attach_type").and_then(|t| t.as_str()) == Some(attach_type))
        .and_then(|link| link.get("id").and_then(serde_json::Value::as_u64))
        .with_context(|| format!("no attached link of type {attach_type} to pin"))
}

/// Whether `err` is aya's pin step failing, as opposed to the load or
/// attach: only then is bpftool the right fallback.
fn is_aya_pin_failure(err: &anyhow::Error) -> bool {
    err.chain()
        .any(|cause| cause.to_string().contains("aya pinning"))
}

fn root_cause(err: &anyhow::Error) -> String {
    err.chain()
        .last()
        .map_or_else(|| err.to_string(), ToString::to_string)
}

/// Removes the pins and the directory: the program detaches when the last
/// reference to its link goes.
fn unpin() {
    let _ = fs::remove_dir_all(PIN_DIR);
}

/// Root's world-writable file for the proofs.
fn make_canary() -> Result<()> {
    let _ = fs::remove_file(CANARY);
    fs::write(CANARY, CANARY_TEXT).with_context(|| format!("creating {CANARY}"))?;
    fs::set_permissions(CANARY, fs::Permissions::from_mode(MODE_WORLD_WRITABLE))
        .with_context(|| format!("making {CANARY} world-writable"))
}

/// Whether `user` can append to the canary.
fn canary_write(user: &str) -> Result<bool> {
    Ok(host::succeeds(Command::new("runuser").args([
        "-u",
        user,
        "--",
        "sh",
        "-c",
        r#"echo probe >> "$1""#,
        "sh",
        CANARY,
    ])))
}

/// One denied write, as the program recorded it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Denial {
    pub ino: u64,
    pub pid: u32,
    pub uid: u32,
    pub owner: u32,
    /// The inode's mode, in octal as `ls` shows it.
    pub mode: String,
    pub mask: u32,
    /// `other` for a write the other bits granted; `acl` where an ACL
    /// applied to the inode too.
    pub reason: String,
    pub comm: String,
}

impl Denial {
    fn parse(bytes: &[u8]) -> Result<Self> {
        ensure!(
            bytes.len() == DENIAL_SIZE,
            "a denial record of {} bytes, not {DENIAL_SIZE}: the program and this reader differ",
            bytes.len()
        );
        let u32_at =
            |at: usize| -> Result<u32> { Ok(u32::from_ne_bytes(bytes[at..at + 4].try_into()?)) };
        let reason = match u32_at(28)? {
            1 => "other".to_owned(),
            2 => "acl".to_owned(),
            other => bail!("a denial with reason {other}, which the reader does not know"),
        };
        let comm = &bytes[32..48];
        let comm = comm
            .iter()
            .position(|b| *b == 0)
            .map_or(comm, |end| &comm[..end]);
        Ok(Self {
            ino: u64::from_ne_bytes(bytes[0..8].try_into()?),
            pid: u32_at(8)?,
            uid: u32_at(12)?,
            owner: u32_at(16)?,
            mode: format!("{:o}", u32_at(20)?),
            mask: u32_at(24)?,
            reason,
            comm: String::from_utf8_lossy(comm).into_owned(),
        })
    }
}

/// Prints the denials recorded so far, one JSON object a line, and
/// drains them: the helper's `world-write-denials`.
pub fn print_denials() -> Result<Exit> {
    let mut out = std::io::stdout().lock();
    for denial in denials()? {
        serde_json::to_writer(&mut out, &denial).context("writing a denial")?;
        writeln!(out).context("writing a denial")?;
    }
    Ok(Exit::Success)
}

/// Parses what [`print_denials`] printed.
pub fn parse_denials(text: &str) -> Result<Vec<Denial>> {
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).with_context(|| format!("parsing a denial: {line}")))
        .collect()
}

/// Reads and drains the denials recorded so far; none where nothing is
/// pinned.
pub fn denials() -> Result<Vec<Denial>> {
    if !Path::new(PIN_DENIALS).exists() {
        return Ok(Vec::new());
    }
    let data = MapData::from_pin(PIN_DENIALS).with_context(|| format!("opening {PIN_DENIALS}"))?;
    let mut ring = RingBuf::try_from(Map::RingBuf(data))
        .with_context(|| format!("{PIN_DENIALS} is not a ring buffer"))?;
    let mut denials = Vec::new();
    while let Some(item) = ring.next() {
        denials.push(Denial::parse(&item)?);
    }
    Ok(denials)
}

/// Whether the attachment is pinned: the ring buffer's pin, which both
/// the aya and the bpftool paths create, stands for it.
pub fn attached() -> bool {
    Path::new(PIN_DENIALS).exists()
}

fn status() -> Result<String> {
    Ok(format!(
        "attached: {}\nbpf in {LSM_LIST}: {}\nmode left by setup: {}",
        if attached() { "yes" } else { "no" },
        if bpf_lsm_active()? { "yes" } else { "no" },
        Mode::on_host()?.name()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn denial_parses_the_record_layout() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&42u64.to_ne_bytes());
        for value in [1234u32, 1001, 0, 0o100666, 2, 1] {
            bytes.extend_from_slice(&value.to_ne_bytes());
        }
        bytes.extend_from_slice(b"sh\0\0\0\0\0\0\0\0\0\0\0\0\0\0");
        assert_eq!(bytes.len(), DENIAL_SIZE);
        let denial = Denial::parse(&bytes).unwrap();
        assert_eq!(
            denial,
            Denial {
                ino: 42,
                pid: 1234,
                uid: 1001,
                owner: 0,
                mode: "100666".to_owned(),
                mask: 2,
                reason: "other".to_owned(),
                comm: "sh".to_owned(),
            }
        );
        assert!(Denial::parse(&bytes[..40]).is_err());
    }
}
