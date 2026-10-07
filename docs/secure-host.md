# Securing the CI host: one setup step for any job

Agent security is just CI security. An agent is one more untrusted step
of a job, like a build script from a pull request or a dependency's
install hook, and what protects a job from one protects it from the
others. So the hardening is a step of its own, `agentic-job sandbox
setup`, that any job can run first, whether or not an agent runs
afterwards; `agentic-job run` builds on it and does not own it.

## What the step does

As root, once, on a machine that is thrown away after the job:

- creates the sandbox user, with its own home and subordinate uids, and
  closes to it the runner's home, the job's directories and everything
  else a runner image leaves open to every local user: world-writable
  system paths, the image's environment file, the hosted compute
  agent's directory, tailscaled's socket;
- takes away that user's sudo, polkit actions, cron, at and lingering,
  and stops the daemons that listen for every local user
  (`sandbox.stop-services`);
- loads network rules keyed on that user's uids, and starts the egress
  proxy that is its only way out;
- installs the caller's packages and the agent programs, and runs the
  caller's script as the sandbox user, for toolchains;
- last, takes root away from the runner's user too: from here on no
  step of the job has root, and the one privileged command left is the
  helper ([sandbox-check.md](sandbox-check.md#after-setup-nothing-has-root)).

`agentic-job sandbox check` then proves each of these as the runner's
user and as the sandbox user, with a positive control for every probe
([sandbox-check.md](sandbox-check.md)).

## A job that runs no agent

CI's own `sandbox` job is one: it installs the binary, runs setup with
[a configuration for the hosted
image](../crates/agentic-job/tests/data/sandbox/ci.toml), runs the
check, and then runs tests as the runner's user, without root, with
every protection it removes put back through a root service it started
before setup. A job of yours does the same:

```yaml
- run: sudo install -m 0755 agentic-job /usr/local/bin/agentic-job
- run: sudo agentic-job sandbox setup --config .github/agentic-job/host.toml
- run: agentic-job sandbox check
# From here on, nothing in this job has root.
```

The configuration is the `[sandbox]`, `[egress]` and `[setup]` tables of
the run's configuration file; the rest may be left out. A step that is
to run as the sandbox user runs through the helper, which is the one
command the runner's user may still run as root:

```yaml
- run: sudo /usr/local/libexec/agentic-job helper enter --chdir /home/runner-sandbox -- make check
```

`run0` wants its standard streams to be sockets where SELinux is
enforcing (the RHEL runners), which a step's pipes are not: there, a
command the step wraps in `socat` or a small program of its own is
needed, as `run` does for the agent. On the hosted Ubuntu runners the
step's pipes work.

## What it does not do

It secures what runs after it. A step before setup runs with the
runner's sudo, and nothing holds a job to the order: that is what a
compiler would add (docs/compiler.md, on the docs pull request), a fixed shape of job in
which only the setup step has root. The reusable workflow has that shape
by construction: every step of its agent job after setup is this
repository's, and a caller's setup script runs as the sandbox user.

It is a uid boundary, not a container: local IPC that answers any user
(the system bus, a daemon's world-connectable socket) crosses it, which
the check finds and the configuration has to stop or accept by name.
And a machine that is not thrown away after the job keeps a runner's
user without root: `sandbox.lock-runner = false` keeps its sudo, at the
price of everything above.
