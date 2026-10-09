'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const vm = require('node:vm');

const root = path.join(__dirname, '..');
const workflow = fs.readFileSync(path.join(root, 'examples/dispatch/dispatch.yml'), 'utf8');
const bounds = fs.readFileSync(path.join(root, 'examples/dispatch/allow.toml'), 'utf8');
const script = workflow.match(/          script: \|\n((?:            .*\n)+)/)?.[1];
assert.ok(script, 'test the actual caller preflight, not a parallel implementation');

async function preflight(env = {}, data = { number: 171 }, ref = 'refs/heads/main') {
  const calls = [];
  const context = {
    process: { env: { PROFILE: 'triage', TARGET_REPO: 'owner/repo', TARGET_ITEM: '171', ...env } },
    context: { ref, payload: { repository: { default_branch: 'main' } } },
    github: { rest: { issues: { get: async args => {
      calls.push(args);
      if (data instanceof Error) throw data;
      return { data };
    } } } },
  };
  await vm.runInNewContext(`(async () => {\n${script}\n})()`, context);
  return calls;
}

for (const profile of ['implement', 'triage', 'research']) {
  test(`${profile} resolves exactly the dispatched issue`, async () => {
    const calls = await preflight({ PROFILE: profile });
    assert.equal(JSON.stringify(calls), JSON.stringify([
      { owner: 'owner', repo: 'repo', issue_number: 171 },
    ]));
  });
}

for (const [name, env, data, ref, error] of [
  ['review', { PROFILE: 'review' }, undefined, undefined, /review is not supported/],
  ['unknown profile', { PROFILE: 'other' }, undefined, undefined, /not supported/],
  ['non-default ref', {}, undefined, 'refs/heads/untrusted', /default branch/],
  ['PR item', {}, { number: 171, pull_request: {} }, undefined, /not a pull request/],
  ['wrong response item', {}, { number: 172 }, undefined, /requires an issue/],
  ['API failure', {}, new Error('not found'), undefined, /not found/],
  ...['0', '-1', '01', '1.5', '1\n', '*', '9007199254740992'].map(item =>
    [`invalid item ${JSON.stringify(item)}`, { TARGET_ITEM: item }, undefined, undefined, /positive safe/]),
  ...['owner/repo/extra', 'owner/repo?x', 'owner/repo\n'].map(repo =>
    [`invalid repo ${JSON.stringify(repo)}`, { TARGET_REPO: repo }, undefined, undefined, /OWNER\/NAME/]),
]) {
  test(`refuse ${name}`, async () => {
    await assert.rejects(preflight(env, data, ref), error);
  });
}

test('dispatch exposes only task, repo, item and kind', () => {
  const inputs = workflow.split('    inputs:\n')[1].split('\npermissions:')[0];
  assert.deepEqual([...inputs.matchAll(/^      ([-\w]+):$/gm)].map(m => m[1]),
    ['task', 'repo', 'item', 'kind']);
});

test('caller fixes capabilities, token boundary and source pin', () => {
  assert.match(workflow, /uses: cgwalters-forge\/agentic-job\/\.github\/workflows\/agentic-job.yml@[a-f0-9]{40}\n/);
  assert.match(workflow, /needs: target/);
  assert.match(workflow, /kind: \$\{\{ inputs.kind == 'implement' && 'branch' \|\| 'analysis' \}\}/);
  assert.match(workflow, /outputs: \$\{\{ inputs.kind == 'implement' && 'create_pull_request,noop,missing_tool,missing_data' \|\| 'add_comment,noop,missing_tool,missing_data' \}\}/);
  for (const line of ["max-outputs: '3'", 'allow: examples/dispatch/allow.toml',
    'comment-target: ${{ inputs.item }}', 'output-repo: ${{ inputs.repo }}',
    'apply-environment: agent-apply', 'apply-partial: false', 'notify: none',
    'agent-runner: ${{ vars.AGENT_RUNNER }}', 'inference-url: ${{ vars.INFERENCE_URL }}',
    'inference-audience: ${{ vars.INFERENCE_AUDIENCE }}']) {
    assert.ok(workflow.includes(line), line);
  }
  assert.doesNotMatch(workflow, /^\s+(secrets:|config:|runner:|contents: write|issues: write|pull-requests: write)/m);
  assert.match(bounds, /^repos = \["cgwalters-forge\/agentic-job"\]$/m);
  assert.match(bounds, /^max_outputs = 3$/m);
  for (const type of ['create_pull_request', 'add_comment', 'noop', 'missing_tool', 'missing_data']) {
    assert.ok(bounds.includes(`${type} = { max = 1 }`), type);
  }
  assert.doesNotMatch(bounds, /create_issue/);
});
