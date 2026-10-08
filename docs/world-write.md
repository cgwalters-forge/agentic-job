# Spike: closing world-writable paths with a BPF program

`sandbox setup` spends most of its time taking the other-write bit off
every path on the root filesystem: on a hosted `ubuntu-26.04` runner,
1m07s to 2m39s to walk 1.03M inodes and `chmod o-w` about 729,000 of them
([#108](https://github.com/cgwalters-forge/agentic-job/issues/108)). The
walk is the protection: afterwards no unprivileged user can write to
anything it does not own through the "other" permission bits. This is a
spike (tracker#405) on doing the same in the kernel, with a BPF program
that denies the write instead of the walk removing the bit. It is on a
branch, not wired into a release.

The headline: it works, on both target systems, by two different kernel
mechanisms; the program is small and loading is delegated to bpftool; but owning
kernel-facing code and the pin's lifecycle are real costs, and the walk
is cheaper to keep than it first looks. The recommendation is at the end.

## The policy

For a non-root fsuid, deny a write that Unix permissions granted only
through the other-write bit of a regular file or directory another uid
owns. Precisely, in `inode_permission` order (DAC first, then the LSM, so
the hook sees only writes DAC already allowed):

- the request has `MAY_WRITE`;
- the inode is a regular file or a directory (not a device, socket or
  fifo), has the other-write bit, and is not sticky (so `/tmp`,
  `/var/tmp`, `/dev/shm` and any sticky directory are untouched);
- the writer's `fsuid` is not 0 and is not the owner;
- the writer is not in the inode's group (group write stays, exactly as
  the walk leaves it: the walk removes only the other bit);
- the inode is not owned by a uid mapped into the writer's user namespace
  (inside a rootless container's userns the owner and writer are both
  subordinate uids of the sandbox user, so the container writes its own
  chowned layers and volumes freely).

This is the exact equivalent of what the walk guarantees, with one thing
the walk cannot do: it also covers files made world-writable **after**
setup (by a later root step, or by a package), which the walk, a one-time
pass, never sees. That is where an LSM is strictly better.

Holes considered and handled: character/block devices, Unix sockets (the
system bus, journald, the egress proxy) and fifos are excluded by inode
type; sticky world-writable directories by the sticky bit; procfs and
sysfs writes are not `MAY_WRITE` on a world-writable regular inode the
way the rule means; ACLs do not bypass the policy (the hook runs after DAC, so an ACL
grant would already have passed, and the walk would have taken the other
bit off anyway, so denying is correct); hard
links are covered because the check is on the target inode; idmapped and
overlay mounts inside the userns are covered by the userns-mapping
exemption. Writes through an already-open fd (`write(2)`, `mmap`,
`ftruncate`) are **not** seen by `inode_permission`; the hook is at open
and at directory operations, which is what the walk's bit also governs
(the bit is checked at open, not at every write), so the coverage matches.

## What each system offers, measured

| | hosted `ubuntu-26.04` | RHEL 10.2 |
| --- | --- | --- |
| kernel | 7.0.0-1012-azure | 6.12.0-211.16.1.el10_2 |
| `/sys/kernel/security/lsm` has `bpf` | no | yes |
| `CONFIG_BPF_LSM` | y | y |
| `CONFIG_DEBUG_INFO_BTF` | y | y |
| `CONFIG_SECURITY_PATH` | y | y |
| lockdown | `[none]` | `[none]` |
| unprivileged bpf | `CONFIG_BPF_UNPRIV_DEFAULT_OFF=y` | same |
| mechanism used | `fmod_ret/security_inode_permission` | `lsm/inode_permission` |

Ubuntu builds `bpf` into the kernel but leaves it out of the boot-time
`CONFIG_LSM` list (`landlock,lockdown,yama,integrity,apparmor`), and the
active list is fixed at boot; adding `bpf` needs `lsm=...,bpf` on the
kernel command line and a reboot, which a hosted runner cannot do. So a
real BPF LSM program attaches there but never runs. The fallback is an
`fmod_ret` (`BPF_MODIFY_RETURN`) program on `security_inode_permission()`:
the verifier allows modify-return on any function whose name starts with
`security_` (`check_attach_modify_return` in `kernel/bpf/verifier.c`,
unchanged at 6.12 and 7.0), independent of `CONFIG_LSM`. It runs before
the real hook and a non-zero return replaces the function's result, so it
denies just as the LSM hook would. Its limits: it needs the function to
be a BTF-known, ftrace-able symbol (it is); it cannot call `bpf_d_path`
(not on the allowlist; `security_file_open` is, if a path were ever
needed); and the `security_` allowance is a 2020 stopgap that upstream has
only ever widened, never proposed to remove, so it is low but not zero
risk a future kernel takes it away.

Other host-wide options on Ubuntu, each rejected:

- **AppArmor** profiles attach to executables, not to every process of a
  user, and the default is unconfined; there is no "deny writes to another
  uid's files" that applies across all of a user's processes, including
  ones systemd starts for it. Not host-wide for this.
- **Landlock** is per-process and inherited, so it would have to be
  applied where the runner's worker starts and would then bind every later
  step; but a Landlock-restricted process cannot change the filesystem
  topology, so `mount` and rootless podman break. Rejected.
- **fanotify** has permission events only for open, access and (6.14+)
  pre-access; there is no permission event for creating a file in a
  directory, so it cannot cover the directory case. Rejected.
- **idmapped / read-only bind mounts of `/`** would change what every
  step sees and break writes the job legitimately makes. Rejected.

## Toolchain

The question "could we write it in Rust?" has two halves.

**The program (kernel side): C, not Rust.** It reads kernel struct fields
(`inode.i_uid/i_mode`, `cred.fsuid/group_info`, `user_namespace.uid_map`)
that differ in layout between 6.12 and 7.0. Portability needs CO-RE: the
loader rewrites each field's offset from the running kernel's BTF. Rust's
eBPF target has no CO-RE relocations yet
([aya-rs/aya#349](https://github.com/aya-rs/aya/issues/349), open since
2022), so a Rust program would bake in one kernel's offsets and also needs
nightly (`-Z build-std`, `bpf-linker` against a matching LLVM). The C
program is 180 lines with a hand-written minimal `vmlinux` header
declaring only the members it reads; `clang -target bpf` builds it with no
nightly. `bpf/world_write.bpf.c` and `bpf/vmlinux_min.h`.

**Loading (userspace side): bpftool alone.** There is no Rust BPF loader,
aya fork or denial ring buffer. Setup invokes `bpftool prog loadall OBJECT
/sys/fs/bpf/agentic-job/links autoattach`, then proves that an actual
non-root write is refused after bpftool exits. No maps need pinning.
The current object has two entry points, so this uses `loadall`, not
`load`, and attaches both: `Hook` specifies a host prerequisite check,
not a program selector. Even `--hook lsm` attaches modify-return, which can
deny before the LSM executes. The CLI therefore reports a **combined-policy**
probe, not proof of either hook independently. Selection of just one entry
point and independent enforcement coverage remain unfinished. Pin presence
is only a status hint, never enforcement proof.

Hosts need bpftool, mounted bpffs, kernel BTF and root for setup, not clang
or a Rust loader. RHEL 10 uses the `bpftool` RPM; the Ubuntu 26.04 spike
workflow explicitly installs the `bpftool` package and `libbpf-dev`/clang
for compilation. This does not establish that bpftool is preinstalled on
the hosted image. Build the external object with:

```sh
clang -O2 -g -target bpf -D__TARGET_ARCH_x86 -Wall -Werror -c crates/agentic-job/bpf/world_write.bpf.c -o world_write.bpf.o
```

The release pipeline still needs to build this object once and publish it
beside the binary. The runner performing this rework cannot modify CI or
release workflows, fetch bpftool/libbpf source, or load BPF as root.
Source verification of autoattach dispatch and the actual pinned link
types on 6.12 and 7.0 remains required; the earlier claim that autoattach
pins programs rather than links must not be treated as established.

## Operational findings

- **Loading is root's, at the end of setup** (the last privileged step),
  pinned under `/sys/fs/bpf/agentic-job`. `/sys/fs/bpf` is mode 1700, so
  the pins are unreachable by non-root; the program outlives the loader.
- **Fail closed, proven.** `load` writes a world-writable file of root's,
  checks a non-root user can write it before attaching and cannot after,
  and fails setup (leaving nothing pinned) if the chosen mechanism does
  not actually deny. This is what catches an LSM program that attached but
  never runs.
- **A false positive breaks every later step, host-wide.** During the
  spike, a program left attached denied a write on the session-setup path
  and bricked new SSH logins to the devspace until it was detached. This
  is the sharp edge of a host-wide deny hook and the reason the
  must-not-break list and the positive controls matter: a bug here is not
  "the agent is blocked", it is "the host is unusable for root too".
- **Detach is removing the pin directory**, after an RCU grace period;
  there is no `link detach` for LSM links.
- **No observability API**: enforcement is proved by a plain write, not
  by reading a denial log.
- **`sandbox check`** reads which mode setup left (`/etc/agentic-job/world-write`)
  and probes accordingly: with the program, the world-writable paths are
  still present, so the probe is that writing one of root's is denied, the
  user's own is not. It first checks that the denied target is still
  root-owned and world-writable.

## Historical measurements (before the bpftool-only rework)

**The mechanism is fast.** On RHEL 10.2 (`cgwalters-devspace-37644059642`,
6.12.0-211.16.1.el10_2), built with `clang` 21.1.8, loaded and attached
with aya, pinned via bpftool: load + CO-RE relocate + attach + pin +
deny-proof is **0.3 s**, against seconds-to-minutes for the walk's own
`chmod`. On Ubuntu via `fmod_ret` it is 0.4 s. A non-root write to a
world-writable root-owned file is denied and recorded; `/dev/null`,
`/dev/zero`, `/tmp`, `/dev/shm`, the system bus, group-writable and
group+world-writable files, sticky-directory creates, and rootless podman
writing its own chowned layers and the user's volume are all allowed, on
both kernels. Removing the pin directory detaches the program and the
write is allowed again.

**But the full setup time barely moves, which is the important number.**
The walk is one `find` over the root filesystem that does two things in a
single pass: `chmod o-w` every world-writable path, and write out the
list of setuid-root programs that setup then checks for unowned ones. The
LSM replaces only the first. Setup still has to traverse every inode for
the setuid scan, and that traversal, not the `chmod`, is what costs
minutes. So on a hosted `ubuntu-26.04` runner the two full-setup times
are within the walk's own cold variance (1m07s-2m39s, #108):

| mode | `sandbox setup`, hosted ubuntu-26.04 |
| --- | --- |
| walk | 105 s |
| lsm (fmod_ret) | 192 s |

The LSM saved the `chmod` (about 13 s by #108's breakdown) and spent it
again on load and on a slightly slower run; it did not remove the walk's
dominant cost. The only way the LSM makes setup meaningfully faster is to
also take over the setuid-root protection so that no full traversal is
needed at all, which is a larger piece of work and a second kernel policy
to own.

These are the original spike's results, not new timings for this change.
The `spike-world-write` workflow still needs its final denial-helper step
removed, since that API no longer exists. Both modes must be rerun in CI
and new timings collected; RHEL attachment also needs a privileged host.

## Recommendation

**Keep the walk.** The case for the LSM was that it is faster, and on the
full setup path it is not: setup still traverses every inode for the
setuid scan, so the LSM saves only the `chmod` (~13 s), not the minutes
(the table above). Its real advantage is coverage, not speed: it also
denies writes to paths made world-writable after setup, which the walk
never sees. That is worth revisiting, but not now, and these costs stand
in the way:

- it secures only the runners where it runs, so Ubuntu and RHEL would be
  protected by two mechanisms, which the docs and probes must track;
- a false positive takes the whole host down, root included, so it needs
  the positive-control test matrix kept green on every kernel update;
- it is kernel-facing code to own across kernel updates (the CO-RE header,
  the BTF dependency, the `security_` prefix allowance on Ubuntu that a
  future kernel could remove);
- the pin lifecycle on current kernels still leans on bpftool.

What would change the recommendation: moving the setuid-root protection
into the LSM too, so setup needs no full traversal and the saving becomes
the whole walk rather than just the `chmod`; a hosted runner image not
built with umask 000 (which shrinks the walk to seconds and makes the LSM
unnecessary); or needing the after-setup coverage badly enough to pay for
kernel-facing code on a RHEL-only deployment.
