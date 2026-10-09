// Real pinned collector -> Rust check -> real pinned handler, with no forge.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdirSync, mkdtempSync, readFileSync, writeFileSync, rmSync } from 'node:fs';
import { homedir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';
import { test } from 'node:test';

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, '../../../..');
const js = resolve(process.env.GH_AW_JS);
const binary = resolve(process.env.AGENTIC_JOB);
const require = createRequire(import.meta.url);
const workflow = readFileSync(join(root, '.github/workflows/agentic-job.yml'), 'utf8');
const configStep = workflow.split('- name: Write the handlers\' configuration\n')[1]
  .split('        run: |\n')[1].split(/\n        [^ ]/)[0]
  .split('\n').filter(line => line.startsWith('          ')).map(line => line.slice(10)).join('\n');

test('checked issue actions preserve the actual handler payload', async () => {
  const repo = 'owner/source';
  const prefix = 'r'.repeat(64);
  for (const [name, labels, max, blocked, ok] of [
    ['ordinary labels', ['triage', 'docs'], 2, [], true],
    ['case preserved', ['TRIAGE'], 1, [], true],
    ['safe punctuation and spaces', ['area/docs: needs_triage-v1.0'], 1, [], true],
    ['64 characters', [prefix], 1, [], true],
    ['punctuation bypass', ['release&'], 1, ['release'], false],
    ['Unicode hardening', ['release\u200b'], 1, ['release'], false],
    ['Unicode normalization', ['ｒｅｌｅａｓｅ'], 1, ['release'], false],
    ['truncation bypass', [prefix + 'x'], 1, [prefix], false],
    ['per-request cap', ['triage', 'docs'], 1, [], false],
    ['bodyless close', null, 1, [], true],
    ['close with body', null, 1, [], false],
  ]) {
    const work = mkdtempSync(join(homedir(), 'handler-test-'));
    try {
      const file = name => join(work, name);
      mkdirSync(file('outputs'));
      const type = labels ? 'add_labels' : 'close_issue';
      const item = labels
        ? { type, repo, item_number: 7, labels }
        : { type, repo, issue_number: 7, ...(name === 'close with body' ? { body: 'Do not silently discard this comment.' } : {}) };
      writeFileSync(file('allow.toml'), `hosts = ["github.com"]\nrepos = ["${repo}"]\nbases = ["main"]\nmax_outputs = 3\nmax_patch_bytes = 100000\nmax_patch_files = 10\n[outputs.${type}]\nmax = ${max}\n` +
        (labels ? `allowed = ${JSON.stringify(labels)}\nblocked = ${JSON.stringify(blocked)}\n` : ''));
      const policyResult = spawnSync(binary, ['policy', '--allow', file('allow.toml'), '--repo', repo,
        '--clone-url', `https://github.com/${repo}`, '--base', 'main', '--kind', 'analysis', '--outputs', type,
        '--max-outputs', String(max)], { encoding: 'utf8' });
      assert.equal(policyResult.status, 0, policyResult.stderr);
      const policy = JSON.parse(policyResult.stdout);
      writeFileSync(file('policy.json'), policyResult.stdout);
      writeFileSync(file('config.json'), JSON.stringify(policy.safe_outputs));
      writeFileSync(file('outputs/outputs.jsonl'), JSON.stringify(item) + '\n');
      const collected = spawnSync(process.execPath, [join(here, 'collect.cjs'), js, file('outputs/outputs.jsonl'),
        file('config.json'), join(root, 'safe-outputs/validation.json'), repo, file('collected.json')], { encoding: 'utf8' });
      assert.equal(collected.status, 0, `${name}: ${collected.stderr}`);
      assert.deepEqual(JSON.parse(readFileSync(file('collected.json'))).errors, [], name);
      const checked = spawnSync(binary, ['check', '--policy', file('policy.json'), '--outputs', file('outputs'),
        '--collected', file('collected.json'), '--report', file('report.json')], { encoding: 'utf8' });
      const verdict = JSON.parse(readFileSync(file('report.json')));
      assert.equal(verdict.ok, ok, `${name}: ${JSON.stringify(verdict.errors)} ${checked.stderr}`);
      assert.equal(checked.status, ok ? 0 : 1, name);
      const configured = spawnSync('bash', ['-euo', 'pipefail', '-c', configStep], { encoding: 'utf8', env: {
        ...process.env, GH_AW_TMP: work, GITHUB_ENV: file('env'), REPO: repo, OUTPUT_REPO: 'owner/other',
        BASE: 'main', BRANCH_PREFIX: '', TITLE_PREFIX: '', PARTIAL: '', COMMENT_TARGET: '', PULL_REQUEST: '{}',
      } });
      assert.equal(configured.status, 0, configured.stderr);
      const calls = [];
      const entity = { number: 7, title: 'test', state: 'open', labels: [], html_url: 'https://example.invalid/7' };
      Object.assign(globalThis, {
        core: { info() {}, warning() {}, debug() {}, error() {}, startGroup() {}, endGroup() {} },
        context: { repo: { owner: 'owner', repo: 'source' }, payload: {}, eventName: 'workflow_dispatch' },
        github: { rest: { issues: {
          get: async () => ({ data: entity }),
          addLabels: async payload => { calls.push(payload); return { data: payload.labels.map(name => ({ name })) }; },
          update: async payload => { calls.push(payload); return { data: { ...entity, state: 'closed' } }; },
          createComment: async () => { assert.fail('closure must not post a comment'); },
        } } },
      });
      if (verdict.ok) {
        const config = JSON.parse(readFileSync(file('handler-config.json')));
        const handler = await require(join(js, `${type}.cjs`)).main(config[type]);
        const result = await handler(verdict.items[0], {});
        assert.equal(result.success, true, `${name}: ${JSON.stringify(result)}`);
        assert.deepEqual(calls, [{ owner: 'owner', repo: 'source', issue_number: 7,
          ...(labels ? { labels } : { state: 'closed', state_reason: 'completed' }) }], name);
        if (!labels) {
          // Defense in depth: even if a body reaches apply, the workflow's
          // explicit configuration must suppress the handler's comment path.
          const direct = await require(join(js, `${type}.cjs`)).main(config[type]);
          const closed = await direct({ ...item, body: 'Ignored by allow_body: false.' }, {});
          assert.equal(closed.success, true, JSON.stringify(closed));
          assert.deepEqual(calls[1], calls[0]);
          assert.equal(calls.length, 2);
        }
      } else {
        assert.deepEqual(calls, [], `${name}: refused outputs never reach apply`);
      }
    } finally {
      rmSync(work, { recursive: true, force: true });
    }
  }
});
