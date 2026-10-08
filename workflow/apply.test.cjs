// Exercise the actual workflow shell, without tokens or a forge.
const assert = require('node:assert/strict');
const { spawnSync } = require('node:child_process');
const { mkdtempSync, mkdirSync, readFileSync, writeFileSync, rmSync } = require('node:fs');
const { join } = require('node:path');
const { homedir } = require('node:os');
const { test } = require('node:test');

const workflow = readFileSync(join(__dirname, '../.github/workflows/agentic-job.yml'), 'utf8');

function step(name) {
  const section = workflow.split(`- name: ${name}\n`)[1];
  assert.ok(section, name);
  return section.split('        run: |\n')[1].split(/\n        [^ ]/)[0]
    .split('\n').filter(line => line.startsWith('          ')).map(line => line.slice(10)).join('\n');
}

function command(cwd, program, args, env = {}) {
  return spawnSync(program, args, { cwd, encoding: 'utf8', env: { ...process.env, ...env } });
}

function git(cwd, ...args) {
  const result = command(cwd, 'git', args);
  assert.equal(result.status, 0, result.stderr);
  return result.stdout.trim();
}

for (const [name, filename, files, unrelated, expected] of [
  ['ordinary edit', 'file.txt', ['file.txt'], false, 0],
  ['CRLF edit', 'file.txt', ['file.txt'], false, 0],
  ['Unicode path', 'café.txt', ['café.txt'], false, 0],
  ['unchecked path', 'file.txt', ['other.txt'], false, 1],
  ['empty file list', 'file.txt', [], false, 1],
  ['unrelated base', 'file.txt', ['file.txt'], true, 1],
]) {
  test(`apply guard: ${name}`, () => {
    const root = mkdtempSync(join(homedir(), 'apply-test-'));
    try {
      const repo = join(root, 'repo');
      const outputs = join(root, 'outputs');
      mkdirSync(repo);
      mkdirSync(outputs);
      git(repo, 'init', '-q');
      git(repo, 'config', 'user.name', 'Test');
      git(repo, 'config', 'user.email', 'test@example.invalid');
      const cr = name === 'CRLF edit' ? '\r' : '';
      writeFileSync(join(repo, filename), `before${cr}\n`);
      git(repo, 'add', '.');
      git(repo, 'commit', '-qm', 'base');
      const base = git(repo, 'rev-parse', 'HEAD');
      git(repo, 'update-ref', 'refs/agentic-job/target-base', base);
      writeFileSync(join(repo, filename), `after${cr}\n`);
      git(repo, 'commit', '-qam', 'edit');
      writeFileSync(join(outputs, 'aw-test.patch'), command(repo, 'git', ['format-patch', '-1', '--stdout']).stdout);
      git(repo, 'reset', '--hard', base);
      if (unrelated) {
        git(repo, 'checkout', '--orphan', 'unrelated');
        git(repo, 'commit', '-qm', 'unrelated root');
        git(repo, 'update-ref', 'refs/agentic-job/target-base', 'HEAD');
      }
      const report = join(root, 'report.json');
      writeFileSync(report, JSON.stringify({ patch: { base_commit: base, files } }));
      const configured = command(repo, 'bash', ['-euo', 'pipefail', '-c', step('Configure git')]);
      assert.equal(configured.status, 0, configured.stderr);
      assert.equal(git(repo, 'config', 'merge.renames'), 'false');
      assert.equal(git(repo, 'config', 'am.keepcr'), 'true');
      const result = command(repo, 'bash', ['-euo', 'pipefail', '-c', step('Apply the patch on its own base, and compare what changed with what was checked')], {
        REPORT: report, RUNNER_TEMP: root, GH_AW_TMP: outputs, REPO: 'test/repo', BASE: 'main',
      });
      if (expected === 0) {
        assert.equal(result.status, 0, result.stderr + result.stdout);
      } else {
        assert.notEqual(result.status, 0, result.stderr + result.stdout);
      }
      if (name === 'CRLF edit') {
        git(repo, 'am', '--3way', join(outputs, 'aw-test.patch'));
        assert.equal(readFileSync(join(repo, filename), 'utf8'), 'after\r\n');
      }
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });
}

test('handler cannot follow a renamed file into a protected path', () => {
  const root = mkdtempSync(join(homedir(), 'apply-test-'));
  try {
    git(root, 'init', '-q');
    git(root, 'config', 'user.name', 'Test');
    git(root, 'config', 'user.email', 'test@example.invalid');
    writeFileSync(join(root, 'file.txt'), 'before\n');
    git(root, 'add', '.');
    git(root, 'commit', '-qm', 'base');
    const base = git(root, 'rev-parse', 'HEAD');
    writeFileSync(join(root, 'file.txt'), 'after\n');
    git(root, 'commit', '-qam', 'edit');
    const patch = join(root, 'mail.patch');
    writeFileSync(patch, command(root, 'git', ['format-patch', '-1', '--stdout']).stdout);
    git(root, 'reset', '--hard', base);
    git(root, 'mv', 'file.txt', '.envrc');
    git(root, 'commit', '-qm', 'rename');
    const configured = command(root, 'bash', ['-eo', 'pipefail', '-c', step('Configure git')]);
    assert.equal(configured.status, 0, configured.stderr);
    const result = command(root, 'git', ['am', '--3way', patch]);
    assert.notEqual(result.status, 0, result.stderr);
    assert.equal(readFileSync(join(root, '.envrc'), 'utf8'), 'before\n');
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
