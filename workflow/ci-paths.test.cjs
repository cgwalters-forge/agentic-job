'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { execFileSync, spawnSync } = require('node:child_process');
const { requiresE2e } = require('./ci-paths.cjs');

const base = 'a'.repeat(40);
const head = 'b'.repeat(40);
const event = { before: base, after: head, pull_request: { base: { sha: base }, head: { sha: head } } };
const root = path.join(__dirname, '..');
const ci = fs.readFileSync(path.join(root, '.github/workflows/ci.yml'), 'utf8');

test('each CI apt operation has acquisition retries and a short step timeout', () => {
  let operations = 0;
  for (const file of ['ci.yml', 'build.yml']) {
    const workflow = fs.readFileSync(path.join(root, '.github/workflows', file), 'utf8');
    assert.match(workflow, /run: sudo node secure-host\/apt\.mjs/);
    for (const match of workflow.matchAll(/^      - run: (sudo apt-get[^\n]*)\n([^]*?)(?=^      - |\s*$)/gm)) {
      operations++;
      assert.ok(workflow.indexOf('run: sudo node secure-host/apt.mjs') < match.index, file);
      assert.match(match[1], /-o Acquire::Retries=3/, file);
      assert.match(match[2], /^        timeout-minutes: 5$/m, file);
    }
  }
  assert.equal(operations, 3);
  const action = fs.readFileSync(path.join(root, 'secure-host/action.yml'), 'utf8');
  assert.ok(action.includes('run: sudo node "$ACTION/apt.mjs"'));
  assert.ok(action.indexOf('run: sudo node "$ACTION/apt.mjs"') < action.indexOf('- name: Get the binary'));
  const binary = fs.readFileSync(path.join(root, 'secure-host/binary.mjs'), 'utf8');
  assert.match(binary, /run\("sudo", \["node", join\(HERE, "apt.mjs"\)\]\);\s+run\("sudo", \["apt-get"/);
});

test('only README and documentation Markdown can skip E2E', () => {
  for (const [names, expected] of [
    [['README.md'], false], [['docs/workflow.md', 'docs/nested/page.md'], false],
    [[], true], [['docs/example.sh'], true], [['workflow/review.md'], true],
    [['AGENTS.md'], true], [['crates/agentic-job/README.md'], true],
    [['.github/workflows/ci.yml'], true], [['secure-host/action.yml'], true],
    [['egress/requirements.txt'], true], [['safe-outputs/validation.json'], true],
    [['README.md', 'crates/agentic-job/src/main.rs'], true],
    [['docs/line\nbreak.md'], true],
    [[...Array.from({ length: 500 }, (_, i) => `docs/${i}.md`), 'workflow/review.toml'], true],
  ]) {
    const output = Buffer.from(names.length ? `${names.join('\0')}\0` : '');
    assert.equal(requiresE2e(event, 'pull_request', () => output), expected, JSON.stringify(names));
  }
});

test('a fork\'s pull request skips E2E, and is held back when its paths need it', () => {
  // ci.yml decides what a fork's pull request is: this file's script runs
  // from the pull request's own tree, and only answers for the paths.
  const fork = "github.event_name == 'pull_request' && github.event.pull_request.head.repo.full_name != github.repository";
  const changes = ci.split('\n  changes:\n')[1].split(/\n  [a-z][a-z-]*:\n/)[0];
  assert.ok(changes.includes(`\n      e2e: \${{ ${fork} && 'false' || steps.paths.outputs.e2e }}\n`));
  // Anything but a plain "no" from the paths holds the pull request back.
  assert.ok(changes.includes(`\n      fork-e2e: \${{ ${fork} && steps.paths.outputs.e2e != 'false' }}\n`));
  assert.doesNotMatch(fs.readFileSync(path.join(__dirname, 'ci-paths.cjs'), 'utf8'), /fork|full_name/);
});

test('CI gives a fork\'s pull request no trigger, secret or OIDC token', () => {
  // A fork's pull request runs on pull_request only, with what GitHub caps
  // for it; a job that holds a write or OIDC permission runs only on push or
  // past the changes job, which turns a fork's pull request away above.
  assert.match(ci, /\non:\n  pull_request:\n  push:\n    branches: \[main\]\n\n/);
  assert.doesNotMatch(ci, /pull_request_target|workflow_run|secrets\./);
  const jobs = ci.split('\njobs:\n')[1].split(/\n(?=  [a-z][a-z0-9-]*:\n)/);
  let privileged = 0;
  for (const job of jobs) {
    // A job's permissions can be an alias of another's, which writes.
    if (!/: write|permissions: \*/.test(job)) continue;
    privileged++;
    assert.match(job, /\n    if: \$\{\{ (always\(\) && )?needs\.changes\.outputs\.e2e == 'true'( && [^|\n]*)? \}\}\n|\n    if: \$\{\{ github\.event_name == 'push' \}\}\n/, job.split('\n')[0]);
  }
  assert.ok(privileged >= 10, String(privileged));
});

test('event SHAs are validated and diff errors run the full suite', () => {
  for (const name of ['pull_request', 'push']) {
    requiresE2e(event, name, args => {
      assert.deepEqual(args, ['diff', '--no-ext-diff', '--no-renames', '--name-only', '-z',
        `${base}${name === 'pull_request' ? '...' : '..'}${head}`, '--']);
      return Buffer.from('README.md\0');
    });
    assert.equal(requiresE2e(event, name, () => { throw new Error('missing history'); }), true);
    assert.equal(requiresE2e(event, name, () => Buffer.from('README.md')), true);
  }
  for (const before of [undefined, '', '0'.repeat(40), '--output=file', 'g'.repeat(40)]) {
    assert.equal(requiresE2e({ ...event, before }, 'push', () => assert.fail('must not run git')), true);
  }
  assert.equal(requiresE2e({}, 'pull_request'), true);
  assert.equal(requiresE2e(event, 'merge_group'), true);
});

test('real Git compares PR history and detects renames out of code', () => {
  const dir = fs.mkdtempSync(path.join(os.homedir(), 'ci-paths-test-'));
  const git = args => execFileSync('git', args, { cwd: dir, stdio: ['ignore', 'pipe', 'pipe'] });
  const commit = () => {
    git(['add', '.']);
    git(['-c', 'user.name=Test', '-c', 'user.email=test@example.invalid', 'commit', '-qm', 'fixture']);
    return git(['rev-parse', 'HEAD']).toString().trim();
  };
  try {
    git(['init', '-q']);
    fs.writeFileSync(path.join(dir, 'code.rs'), 'code');
    const first = commit();
    fs.mkdirSync(path.join(dir, 'docs'));
    fs.writeFileSync(path.join(dir, 'docs', 'page.md'), 'docs');
    const second = commit();
    const pr = { pull_request: { base: { sha: first }, head: { sha: second } } };
    assert.equal(requiresE2e(pr, 'pull_request', git), false);
    fs.renameSync(path.join(dir, 'code.rs'), path.join(dir, 'docs', 'code.md'));
    const third = commit();
    assert.equal(requiresE2e({ before: second, after: third }, 'push', git), true);
    assert.equal(requiresE2e({ before: head, after: third }, 'push', git), true);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test('required ci check permits only explicitly gated skips', () => {
  // Exercise the actual workflow expression, not a second implementation.
  const command = ci.slice(ci.indexOf('        run: >-\n', ci.indexOf('\n  ci:')))
    .replace('        run: >-\n', '').trim().split('\n').map(line => line.trim()).join(' ');
  const skipped = ['e2e-full', 'e2e-limit', 'e2e-event', 'review-base', 'e2e-review', 'e2e-dispatch', 'e2e-compose', 'e2e-dispatch-verify', 'e2e-verify'];
  const normal = ['changes', 'docs', 'rust', 'sandbox'];
  const needs = Object.fromEntries([...normal, ...skipped].map(key => [key, { result: 'success' }]));
  const run = (state, e2e, installed = 'true', forkE2e = 'false') => spawnSync('bash', ['-c', command], {
    env: { ...process.env, NEEDS: JSON.stringify(state), E2E: e2e, REVIEW_INSTALLED: installed, FORK_E2E: forkE2e },
    encoding: 'utf8',
  }).status;
  assert.equal(run(needs, 'true'), 0);
  const docsOnly = { ...needs, ...Object.fromEntries(skipped.map(key => [key, { result: 'skipped' }])) };
  assert.equal(run(docsOnly, 'false'), 0);
  for (const e2e of ['true', '', 'unknown']) assert.notEqual(run(docsOnly, e2e), 0);
  // A fork's pull request that needs the e2e jobs skips them all and fails,
  // as does one whose answer is missing.
  for (const forkE2e of ['true', '', 'unknown']) {
    assert.notEqual(run(docsOnly, 'false', 'true', forkE2e), 0, forkE2e);
    assert.notEqual(run(needs, 'true', 'true', forkE2e), 0, forkE2e);
  }
  for (const key of [...normal, ...skipped]) {
    for (const result of ['failure', 'cancelled']) {
      assert.notEqual(run({ ...docsOnly, [key]: { result } }, 'false'), 0, `${key}: ${result}`);
    }
  }
  for (const key of normal) assert.notEqual(run({ ...docsOnly, [key]: { result: 'skipped' } }, 'false'), 0);
  assert.equal(run({ ...needs, 'e2e-review': { result: 'skipped' } }, 'true', 'false'), 0);
  assert.notEqual(run({ ...needs, 'e2e-review': { result: 'skipped' } }, 'true'), 0);
  for (const key of ['e2e-dispatch', 'e2e-compose', 'e2e-dispatch-verify']) {
    assert.notEqual(run({ ...needs, [key]: { result: 'skipped' } }, 'true'), 0);
  }
});

test('all E2E callers and verifier are gated, not workflow triggers', () => {
  for (const job of ['e2e-full', 'e2e-limit', 'e2e-event', 'e2e-analysis', 'e2e-analysis-refused', 'review-base', 'e2e-review', 'e2e-dispatch', 'e2e-compose', 'e2e-dispatch-verify', 'e2e-verify']) {
    const block = ci.split(`\n  ${job}:\n`)[1].split(/\n  [a-z][a-z-]*:\n/)[0];
    assert.match(block, /needs:.*changes/);
    assert.match(block, /if:.*needs\.changes\.outputs\.e2e == 'true'/);
  }
  assert.doesNotMatch(ci, /paths-ignore:|paths:/);
  assert.match(ci, /cancel-in-progress: \$\{\{ github\.event_name == 'pull_request' \}\}/);
  const review = fs.readFileSync(path.join(root, '.github/workflows/review.yml'), 'utf8');
  assert.match(review, /cancel-in-progress: \$\{\{ github\.event_name == 'pull_request_target' \}\}/);
  assert.match(review, /group: review-pull-\$\{\{ github\.event\.pull_request\.number \|\| github\.event\.issue\.number \}\}/);
});
