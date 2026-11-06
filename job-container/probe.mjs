import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

function run(command, args) {
  const result = spawnSync(command, args, { encoding: 'utf8' });
  assert.ifError(result.error);
  assert.equal(result.status, 0, `${command}: ${result.stdout}\n${result.stderr}`);
  return result.stdout.trim();
}

function check(name, fn) {
  try {
    fn();
    console.log(`PASS: ${name}`);
  } catch (error) {
    console.error(`FAIL: ${name}: ${error.message}`);
    process.exitCode = 1;
  }
}

check('step is uid/gid 1001, not root; sudo is absent', () => {
  assert.equal(process.getuid(), 1001);
  assert.equal(process.getgid(), 1001);
  const result = spawnSync('sudo', ['-n', 'true']);
  assert.equal(result.error?.code, 'ENOENT');
});

for (const unshare of [false, true]) {
  check(`docker socket is inaccessible${unshare ? ' inside podman unshare' : ''}`, () => {
    const socket = '/var/run/docker.sock';
    assert.ok(existsSync(socket), 'expected host docker socket is missing');
    const args = ['--silent', '--show-error', '--verbose', '--max-time', '10', '--unix-socket', socket,
      'http://localhost/_ping'];
    const result = unshare
      ? spawnSync('podman', ['unshare', 'curl', ...args], { encoding: 'utf8' })
      : spawnSync('curl', args, { encoding: 'utf8' });
    assert.ifError(result.error);
    // curl 7 is connection failure; require EACCES, not a broken daemon or helper.
    assert.equal(result.status, 7, result.stderr);
    assert.match(result.stderr, /Permission denied/i);
  });
}

check('actions/checkout produced a usable, writable workspace', () => {
  assert.ok(run('git', ['rev-parse', '--is-inside-work-tree']) === 'true');
  assert.equal(run('git', ['rev-parse', 'HEAD']), process.env.GITHUB_SHA);
  const path = mkdtempSync(join(process.cwd(), 'checkout-write-'));
  rmSync(path, { recursive: true });
});

check('Podman selects the intended configuration despite GitHub HOME', () => {
  assert.equal(process.env.CONTAINERS_CONF, '/etc/job-container/containers.conf');
  assert.equal(process.env.CONTAINERS_STORAGE_CONF, '/etc/job-container/storage.conf');
  const info = JSON.parse(run('podman', ['info', '--format', 'json']));
  assert.equal(info.store.configFile, process.env.CONTAINERS_STORAGE_CONF);
  assert.equal(info.store.graphDriverName, 'overlay');
  assert.equal(info.store.graphOptions['overlay.mount_program'].Executable, '/usr/bin/fuse-overlayfs');
  assert.equal(info.store.graphRoot, '/home/runner/.local/share/containers/storage');
  assert.equal(info.store.runRoot, '/tmp/run-user-1001/containers');
  assert.equal(info.host.cgroupManager, 'cgroupfs');
  assert.equal(info.host.eventLogger, 'file');
  assert.equal(info.host.ociRuntime.name, 'crun');
});

check('podman runs a real Fedora image', () => {
  assert.equal(run('podman', ['run', '--rm', '--network=host',
    'registry.fedoraproject.org/fedora:44', 'sh', '-ec',
    '. /etc/os-release; test "$ID" = fedora; printf fedora']), 'fedora');
});

check('podman builds and runs an image with a second user', () => {
  const directory = mkdtempSync(join(tmpdir(), 'podman-build-'));
  try {
    writeFileSync(join(directory, 'Containerfile'),
      'FROM registry.fedoraproject.org/fedora:44\n' +
      'RUN useradd --uid 2000 --create-home second\n' +
      'USER second\nRUN test "$(id -u)" = 2000 && touch /home/second/built\n' +
      'CMD ["sh", "-ec", "test -f /home/second/built; test $(id -u) = 2000; touch /home/second/ran; printf second"]\n');
    run('podman', ['build', '--network=host', '-t', 'localhost/job-container-second', directory]);
    assert.equal(run('podman', ['run', '--rm', '--network=host',
      'localhost/job-container-second']), 'second');
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});

check('process list is confined to the job container', () => {
  // GitHub starts its job container with --entrypoint tail, not the host init.
  const init = readFileSync('/proc/1/cmdline', 'utf8').split('\0').filter(Boolean);
  assert.match(init[0], /(^|\/)tail$/);
  assert.deepEqual(init.slice(1), ['-f', '/dev/null']);
  const processes = run('ps', ['-e', '-o', 'pid=,comm=,args=']);
  console.log(processes);
  assert.doesNotMatch(processes, /\b(systemd|dockerd|containerd|Runner\.Listener)\b/);
});
