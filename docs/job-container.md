# Non-root job container

`job-container/Containerfile` builds a Fedora image with uid and gid 1001,
matching the hosted runner's workspace owner. Every GitHub step, including
checkout and JavaScript actions, runs as that user. Podman uses subordinate
uid/gid ranges, crun and fuse-overlayfs to run and build nested containers.
Only `newuidmap` and `newgidmap` retain file capabilities; the final image
audit rejects any setuid/setgid file or any other file capability. There is
no sudo. Other packaged account and mount helpers have their privileged bits
stripped rather than being trusted as routes to root.

Both configuration files live in `/etc/job-container` and are selected with
`CONTAINERS_CONF` and `CONTAINERS_STORAGE_CONF`, independently of GitHub's
override of `HOME` to `/github/home`. The probe checks the effective storage
configuration, overlay driver, fuse-overlayfs executable, graph/run roots,
cgroup manager, event logger and OCI runtime, not just successful execution.

The workflow template is [`job-container/workflow.yml`](../job-container/workflow.yml).
It is **not active**: a maintainer must copy it to
`.github/workflows/job-container.yml`. This runner is forbidden to modify
that directory. Pushes to main build and publish a commit-tagged image to
this repository's GHCR, then probe that exact image in a fresh job container.
PRs build and audit without publishing; the nested runtime probes run only
after publication. Set the GHCR package's Actions access to allow this
repository to read it if it is private.

This is an image **prototype**, not completion of #137. The template does not
provide pre-merge hosted job-container validation of PR candidates. Installing
it alone does not resolve that gap: a maintainer must also arrange candidate
image delivery without giving untrusted PR jobs publication credentials, make
the hosted probe required before merge, and link a successful run for the exact
image. Publication currently precedes hosted validation; do not treat the
published tag as validated until the probe succeeds.

The job uses exactly these container options:

```text
--security-opt seccomp=unconfined --security-opt apparmor=unconfined --security-opt systempaths=unconfined --device /dev/fuse
```

The probe emits explicit PASS/FAIL lines for the non-root identity, absence
of sudo, denial of the host Docker socket both directly and under
`podman unshare`, writable checkout, real Fedora execution, building and
running as a second user, and a container-local process list. The socket
probe requires an actual permission-denied error, not just a missing socket
or an unavailable daemon. The process probe requires GitHub's container
`tail -f /dev/null` as PID 1 and rejects known host services; it is a smoke
test of PID isolation, not an exhaustive security proof.

## Boundaries and evidence

This is not the host provisioning or network-policy mechanism in
[`secure-host`](secure-host.md). Nothing in a job-container step can provision
the host before that container starts. There is **no network policy** here;
the workload has unrestricted network access. Seccomp and AppArmor are off
for the workload to permit nested rootless Podman. The runner temp directory
and world-writable tool cache are shared. The Docker socket is still mounted
by GitHub; its permissions, not its absence, keep it out of reach. Do not
give this user another host group, privileged mounts, or root credentials.
Nested user-namespace root is not host root, but the workload still shares
the host kernel and this is not a VM boundary.

The prior hosted ubuntu-26.04 results are in
[tracker issue #445, “Probes: GitHub job containers”](https://github.com/cgwalters-forge/tracker/issues/445)
and the
[trial probe workflows](https://github.com/cgwalters-bot/agentic-job-trial/tree/main/.github/workflows)
(`container-probe*.yml`). They established the job-container mechanism and
nested rootless Podman; this image's uid 1001 and workspace compatibility
still need the new CI run. The image was not built or run on the development
machine. Only hosted CI can establish checkout compatibility, mapping-helper
capabilities through GitHub's container setup, nested builds, Docker socket
denial and the observed process boundary for this image.
