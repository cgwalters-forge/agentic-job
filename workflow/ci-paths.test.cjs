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

test('draft cleanup accepts only successful closure or confirmed closed state', () => {
  const step = ci.split('      - name: Close the drafts\n')[1].split('      # From the scratch issue')[0];
  const command = step.split('        run: |\n')[1].trimEnd()
    .split('\n').map(line => line.replace(/^          /, '')).join('\n');
  for (const [name, prs, listExit, patchExit, state, readExit, success, reads] of [
    ['no drafts', '', 0, 0, 'open', 0, true, 0],
    ['closed by PATCH', '12 13', 0, 0, 'open', 0, true, 0],
    ['branch deletion raced with PATCH', '12 13', 0, 1, 'closed', 0, true, 2],
    ['PATCH failed and still open', '12 13', 0, 1, 'open', 0, false, 1],
    ['state read failed', '12', 0, 1, 'closed', 1, false, 1],
    ['listing failed', '', 1, 0, 'closed', 0, false, 0],
  ]) {
    const mock = `gh() {
      if [ "$1" = pr ]; then
        printf '%s\\n' "$PRS"
        return "$LIST_EXIT"
      fi
      if [ "$2" = -X ]; then
        printf 'patch %s\\n' "$4" >&2
        return "$PATCH_EXIT"
      fi
      printf 'read %s\\n' "$2" >&2
      printf '%s\\n' "$STATE"
      return "$READ_EXIT"
    }`;
    const result = spawnSync('bash', ['-e', '-o', 'pipefail', '-c', `${mock}\n${command}`], {
      encoding: 'utf8',
      env: { ...process.env, GH_REPO: 'owner/repo', MINE: 'agent-run-1',
        PRS: prs, LIST_EXIT: String(listExit), PATCH_EXIT: String(patchExit),
        STATE: state, READ_EXIT: String(readExit) },
    });
    assert.ifError(result.error);
    assert.equal(result.status === 0, success, `${name}: ${result.stderr}`);
    assert.equal((result.stderr.match(/^read /gm) || []).length, reads, name);
    if (!success && prs) {
      assert.doesNotMatch(result.stderr, /issues\/13/, `${name}: must stop on a real failure`);
    }
  }
});

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
  const skipped = ['e2e-full', 'e2e-limit', 'e2e-event', 'review-base', 'e2e-review', 'e2e-dispatch', 'e2e-dispatch-verify', 'e2e-verify'];
  const normal = ['changes', 'docs', 'rust', 'sandbox'];
  const needs = Object.fromEntries([...normal, ...skipped].map(key => [key, { result: 'success' }]));
  const run = (state, e2e, installed = 'true', fork = 'false') => spawnSync('bash', ['-c', command], {
    env: { ...process.env, NEEDS: JSON.stringify(state), E2E: e2e, REVIEW_INSTALLED: installed, FORK: fork },
    encoding: 'utf8',
  }).status;
  assert.equal(run(needs, 'true'), 0);
  const docsOnly = { ...needs, ...Object.fromEntries(skipped.map(key => [key, { result: 'skipped' }])) };
  assert.equal(run(docsOnly, 'false'), 0);
  for (const e2e of ['true', '', 'unknown']) assert.notEqual(run(docsOnly, e2e), 0);
  for (const key of [...normal, ...skipped]) {
    for (const result of ['failure', 'cancelled']) {
      assert.notEqual(run({ ...docsOnly, [key]: { result } }, 'false'), 0, `${key}: ${result}`);
    }
  }
  for (const key of normal) assert.notEqual(run({ ...docsOnly, [key]: { result: 'skipped' } }, 'false'), 0);
  assert.equal(run({ ...needs, 'e2e-review': { result: 'skipped' } }, 'true', 'false'), 0);
  assert.notEqual(run({ ...needs, 'e2e-review': { result: 'skipped' } }, 'true'), 0);
  for (const key of ['e2e-dispatch', 'e2e-dispatch-verify']) {
    assert.equal(run({ ...needs, [key]: { result: 'skipped' } }, 'true', 'true', 'true'), 0);
    assert.notEqual(run({ ...needs, [key]: { result: 'skipped' } }, 'true'), 0);
  }
});

test('all E2E callers and verifier are gated, not workflow triggers', () => {
  for (const job of ['e2e-full', 'e2e-limit', 'e2e-event', 'e2e-analysis', 'e2e-analysis-refused', 'review-base', 'e2e-review', 'e2e-dispatch', 'e2e-dispatch-verify', 'e2e-verify']) {
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
