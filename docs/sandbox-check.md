# What `sandbox check` proves

`agentic-job sandbox check` runs as the runner's user after `sandbox
setup`, and `run` repeats it once the run token is in place. Each probe
is something the sandbox user tries and must not manage. Each has a
positive control, the same attempt somewhere it should work, so that a
probe which fails because a program is missing does not read as a
protection that holds. A probe or a control that comes out wrong fails
the check: exit state 1, and for `run`, 4.

This page covers the first half of step 5 of the plan: everything but
the network rules and the egress proxy, whose probes arrive with them.

## The probes

Every line of output starts with `ok` or `FAIL` and the probe's name in
brackets. The names are stable; the test that removes protections
matches on them.

| Probe | The sandbox user tries to | Control | What stops it |
| --- | --- | --- | --- |
| `sudo` | run `sudo -n true` | the runner's user can | a sudoers rule, read last |
| `polkit` | run a command through `pkexec`, from a shell | root can | a polkit rule, read first |
| `groups` | (none: lists its groups) be in a group that is root by another name, such as `docker` or `adm` | it lists its groups | setup gives it none of them, and refuses an image's user that has one |
| `linger` | keep its user manager alive after its sessions (`loginctl enable-linger`) | logind lists its sessions | the polkit rule |
| `environ-self`, `environ-runner` | read the environment of the checking process, and of a second process of the runner's that holds a canary | the runner reads the canary there | the separate uid |
| `environ-canary`, `environ-job-variables` | find the canary, or `ACTIONS_` in any variable, in any environment it can read | it reads its own processes' environments | `run0` passes on none of the caller's environment |
| `env-job-variables`, `oidc-value:NAME` | see `ACTIONS_*` variables in its own environment, or the values of the identity-token request variables in any process it can read | it runs `env`; the checking process has the variables | the same, and refusal by name |
| `private-dir:DIR` | list or enter the runner's home, `/opt/hca`, tailscaled's socket directory, each of `sandbox.private-dirs`, and the job's work directories if they are elsewhere | the runner lists its home | mode 0700 |
| `world-writable` | find anything world-writable on the root filesystem (that one filesystem) outside `/tmp` and `/var/tmp` | it finds a directory made for it | setup removes world write |
| `ptrace-scope` | (none: reads the kernel setting) | | setup raises it to 1 |
| `cron`, `at` | install a crontab; use `at` | the runner's user may | `/etc/cron.deny`, `/etc/at.deny` |
| `service:UNIT` | (none: asks systemd) | journald is active | `sandbox.stop-services` |
| `local-sockets`, `local-tcp` | connect to every listening Unix socket and local TCP port | it finds and reaches two listeners opened for it | stopping the daemon, or naming the socket as allowed |
| `tailscale-status`, `tailscale-localapi` | use tailscaled's LocalAPI | root can | its socket directory is closed |
| `token-runner-file` | read the runner's copy of the run token | the runner reads it | the runner's home is closed |
| `token-config-mode`, `token-config-dir-mode` | (none: the agent's configuration must be its own, mode 600, in a directory of mode 700) | the configuration holds the token | `run` writes it so |
| `token-files`, `token-processes` | find the token in any other file it can write, or in any environment or command line it can read | as above | `run` hands the token over on standard input only |
| `token-container-subuid` | read the configuration from a rootless container as a subordinate uid | the container's root reads it | the modes above |

`network-control`, `container-pull` and `container-control` are controls
only, for the network probes that follow: the sandbox user reaches a
public page, on the host and from a rootless container on the host's
network.

Three of these are new against the old tree's
`agent-isolation-check.mjs`, which had no probe for them: `cron`, `at`
and `linger`; `world-writable`; and `local-sockets` with `local-tcp`.

## The job's work directories

The runner's home is closed, and on the runners we know the job's work
and temporary directories are inside it. Where they are not, they are
one more place the runner's files are. `sandbox setup` runs under sudo
and does not see the job's variables, so this is a probe and not a
refusal in setup: `sandbox check` also tries the directory it runs in,
and those named by `RUNNER_TEMP`, `RUNNER_WORKSPACE` and
`GITHUB_WORKSPACE`, when they are the runner's own and outside what
setup closed. A caller with such a layout lists them in
`sandbox.private-dirs`. These three names are, with the identity-token
request variables, all the binary knows of a CI system.

## Local sockets

The uid boundary covers files and processes. A daemon that listens
where any user may connect is on the other side of it all the same: a
container daemon, a package manager's API, an SSH server. So the check
starts a copy of this program as the sandbox user, which reads
`/proc/net/unix` and `/proc/net/tcp`, tries each listener, and reports
what let it in. An abstract socket has no permissions and is reported
without a try.

What may be there without failing the check is a short list in
`sandbox/check.rs`: the system bus, the journal's and the user
database's sockets, PID 1's notification socket and the Varlink
services directly under `/run/systemd/`, the randomly named datagram
sockets systemd's daemons take notifications on, polkit's password
helper, and the user's own under `/run/user/UID/`. systemd-resolved's
sockets are deliberately not on it: it resolves names for any user, as
itself. Anything else fails the check until the caller either stops the
daemon with `sandbox.stop-services` or names the socket in
`sandbox.check.allow-sockets` or `allow-tcp-ports`. Either way it is a
line in the caller's configuration that someone decided.

It does not see UDP, a listener bound only to an IPv6 link-local
address, nor a service that only listens on another machine. The system
bus is one socket with many services behind it; polkit denies the
sandbox user every action, and a service that asks polkit nothing is
not seen by this probe.

## Where each probe has been seen to fail

`crates/agentic-job/tests/sandbox_host.rs` removes one protection at a
time, as root, and requires that exactly the probes guarding it fail.
CI's `sandbox` job runs it on a hosted `ubuntu-26.04` runner, after
`sandbox setup` and a clean `sandbox check` there. It grants the
sandbox user a sudoers rule (and one that sorts before the deny rule,
which must change nothing); adds it to a group; moves the polkit rule
away, alone and with a rule that grants; opens the runner's home, and
leaves it searchable only; works in a directory outside it; puts a job
variable into the system manager's environment, and the request
token's value into a sandbox process; takes the user off the cron and
at deny lists; makes a file in `/etc` world-writable; resets ptrace;
starts a stopped unit; opens two listeners; and, for the run token,
loosens the configuration's modes, leaves a copy in `/tmp`, puts the
token on a command line, and puts the runner's copy where anyone reads
it.

`tests/sandbox_fresh.rs` runs before setup in the same job: setup
refuses a user that exists, an image's user in a forbidden group, a
home left without its user and a group that does not exist, each
before it has changed anything. That setup goes on to use an image's
user when told to is not exercised.

The first time that test ran it found a probe that passed for the wrong
reason: `pkexec` started straight from `run0` refuses because its
parent is PID 1, whatever polkit says.

What the hosted runner cannot show:

- `environ-self` and `environ-runner` rest on the kernel's own check
  of who may read `/proc/PID/environ`. There is nothing to remove; the
  controls are all the evidence.
- It has no tailscaled. The test removes that protection when it finds
  the socket, and did on a RHEL 10 runner by hand (2026-10-07), where
  all of it passed under SELinux. Step 8 repeats it there.
- It runs AppArmor, not SELinux. That `run0` needs sockets for the
  standard streams under SELinux is exercised only on RHEL.
- `/dev/kvm`: the sandbox user is put in the `kvm` group, and nothing
  here starts a VM as it.

The same RHEL 10 runner image has listeners of its own that the caller
there has to stop or allow in step 8: cockpit on port 9090, sshd, the
sssd credential cache, `lsmd`, iscsid, and the SELinux troubleshooter,
which starts on the first denial.
