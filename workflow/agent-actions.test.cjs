// Exercise the extracted actions' actual shell without changing this host.
const assert = require('node:assert/strict');
const { readFileSync, mkdtempSync, rmSync } = require('node:fs');
const { homedir } = require('node:os');
const { join } = require('node:path');
const { spawnSync } = require('node:child_process');
const { test } = require('node:test');

const prepare = readFileSync(join(__dirname, '../prepare/action.yml'), 'utf8');
const run = readFileSync(join(__dirname, '../run/action.yml'), 'utf8');
const wrapper = readFileSync(join(__dirname, '../.github/workflows/agentic-job.yml'), 'utf8');

function step(source, name) {
  const section = source.split(`- name: ${name}\n`)[1];
  assert.ok(section, name);
  return section.split('      run: |\n')[1].split(/\n    [^ ]/)[0].split('\n')
    .filter(line => line.startsWith('        ')).map(line => line.slice(8)).join('\n');
}

function shell(script, env) {
  return spawnSync('bash', ['-eo', 'pipefail', '-c', script], {
    encoding: 'utf8', env: { ...process.env, ...env },
  });
}

test('prepare rejects missing and nonnumeric trusted artifact IDs before download', () => {
  for (const [binary, policy, expected] of [
    ['1', '2', 0], ['', '2', 1], ['1', '', 1], ['0', '2', 1],
    ['1,2', '3', 1], ['1', 'policy-name', 1], ['$(exit 99)', '2', 1],
  ]) {
    const result = shell(step(prepare, 'Require trusted upload IDs'), { BINARY: binary, POLICY: policy });
    assert.equal(result.status, expected, result.stdout + result.stderr);
  }
  assert.match(prepare, /artifact-ids: \$\{\{ inputs.binary-artifact-id \}\},\$\{\{ inputs.policy-artifact-id \}\}/);
  assert.doesNotMatch(prepare, /name: \$\{\{ inputs\./);
});

test('run rejects unlocked configuration before probing and probes before agent execution', () => {
  const guard = step(run, 'Prove the host is secured');
  const python = guard.match(/python3 -c '([^']+)'/)[1];
  // Replace only the root-owned file read with an in-memory configuration.
  for (const [sandbox, expected] of [[{}, 0], [{ 'lock-runner': true }, 0],
    [{ 'lock-runner': false }, 1], [{ 'lock-runner': 'true' }, 1]]) {
    const code = python.replace('tomllib.load(open("/etc/agentic-job/config.toml", "rb"))',
      `__import__("json").loads(${JSON.stringify(JSON.stringify({ sandbox }))})`);
    const result = spawnSync('python3', ['-c', code], { encoding: 'utf8' });
    assert.equal(result.status, expected, result.stderr);
  }
  const result = shell('python3() { return 1; }; agentic-job() { echo unexpected; };\n' + guard);
  assert.equal(result.status, 1);
  assert.doesNotMatch(result.stdout, /unexpected/);
  assert.ok(run.indexOf('name: Prove the host') < run.indexOf('name: Run the agent'));
});

test('agent exit is captured before uploads and partial result gate is unchanged', (t) => {
  const directory = mkdtempSync(join(homedir(), 'agent-actions-'));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  for (const exit of [0, 1, 3, 4, 124]) {
    const output = join(directory, `output-${exit}`);
    const result = shell('agentic-job() { printf "%s\\n" "$@"; return "$STATE"; };\n' +
      step(run, 'Run the agent'), { JOB_DIR: directory, REVIEW_HEAD: 'review-head', STATE: String(exit),
        GITHUB_OUTPUT: output, GITHUB_STEP_SUMMARY: join(directory, 'summary') });
    assert.equal(result.status, 0, result.stderr);
    assert.equal(readFileSync(output, 'utf8'), `exit=${exit}\n`);
    assert.match(result.stdout, /--review-head\nreview-head/);
    for (const partial of ['false', 'true']) {
      const gate = shell(step(run, "The run's result"), { EXIT: String(exit), PARTIAL: partial });
      const expected = exit === 0 || partial === 'true' && [3, 124].includes(exit) ? 0 : 1;
      assert.equal(gate.status, expected, gate.stdout + gate.stderr);
    }
  }
  assert.ok(run.indexOf('id: safe-outputs') < run.indexOf("name: The run's result"));
  assert.match(run, /inputs.apply-partial == 'true' && steps.safe-outputs.outputs.artifact-id != ''/);
});

test('public visibility refusal blocks publication and repository names are data', () => {
  const script = step(run, 'Check again that the repositories are public');
  for (const [repo, repos, publicRepo, expected] of [
    ['owner/repo', 'owner/repo', 'true', 0], ['owner/repo', 'owner/repo owner/config', 'true', 0],
    ['owner/repo', 'owner/repo', 'false', 1], ['$(exit 99)', '$(exit 99)', 'true', 1],
    ['', '', 'true', 1], [' ', ' ', 'true', 1], ['owner/repo', 'owner/repo invalid', 'true', 1],
  ]) {
    const result = shell('gh() { printf "%s\\n" "$PUBLIC"; };\n' + script,
      { REPO: repo, REPOS: repos, PUBLIC: publicRepo });
    assert.equal(result.status, expected, result.stdout + result.stderr);
  }
  assert.ok(run.indexOf('Check again that') < run.indexOf('uses: actions/upload-artifact'));
});

test('wrapper uses the same pinned-source producer and keeps trusted edges off agent outputs', () => {
  const agent = wrapper.split('\n  agent:\n')[1].split('\n  notify:\n')[0];
  assert.match(agent, /uses: \.\/\.agentic-job-source\/prepare/);
  assert.match(agent, /uses: \.\/\.agentic-job-source\/run/);
  assert.match(agent, /safe-outputs-artifact-id: \$\{\{ steps.run.outputs.safe-outputs-artifact-id \}\}/);
  for (const input of ['binary-artifact-id', 'policy-artifact-id']) {
    assert.ok(agent.includes(`${input}: \${{ needs.policy.outputs.${input} }}`));
  }
  assert.match(agent, /repository: \$\{\{ job.workflow_repository \}\}/);
  assert.match(agent, /ref: \$\{\{ job.workflow_sha \}\}/);
  assert.match(agent, /contents: read[\s\S]*id-token: write/);
  for (const action of [prepare, run]) {
    assert.doesNotMatch(action, /secrets\.|SAFE_OUTPUTS_PAT/);
    for (const use of action.matchAll(/uses: ([^\n]+)/g)) {
      assert.match(use[1], /^actions\/[\w-]+@[0-9a-f]{40} /);
    }
  }
  assert.ok(agent.indexOf('/prepare') < agent.indexOf('/secure-host'));
  assert.ok(agent.indexOf('/secure-host') < agent.indexOf('/run'));
});
