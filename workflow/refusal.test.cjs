// Exercise the trusted workflow scripts without a forge or token.
const assert = require('node:assert/strict');
const { spawnSync } = require('node:child_process');
const fs = require('node:fs');
const { join } = require('node:path');
const { homedir } = require('node:os');
const { test } = require('node:test');

const workflow = fs.readFileSync(join(__dirname, '../.github/workflows/agentic-job.yml'), 'utf8');

function script(name, key) {
  const section = workflow.split(`- name: ${name}\n`)[1].split(/\n      - /)[0];
  const indent = key === 'script' ? 12 : 10;
  return section.split(`${key}: |\n`)[1].split('\n')
    .filter(line => line.startsWith(' '.repeat(indent))).map(line => line.slice(indent)).join('\n');
}

for (const phase of ['collector', 'policy', 'review']) {
  test(`${phase} refusal reaches annotations and the conclusion comment`, async () => {
    const dir = fs.mkdtempSync(join(homedir(), 'workflow-refusal-'));
    const source = join(__dirname, '..');
    const env = { ...process.env, GH_AW_TMP: dir, SOURCE_DIR: source, TARGET: '12' };
    const errors = [];
    const outputs = {};
    const core = { error: text => errors.push(text), setFailed: text => errors.push(text),
      setOutput: (key, value) => { outputs[key] = value; } };
    const AsyncFunction = Object.getPrototypeOf(async function () {}).constructor;
    try {
      fs.writeFileSync(join(dir, 'agent_output.json'), JSON.stringify({
        errors: phase === 'collector' ? ['collector refused'] : [], items: [],
      }));
      if (phase === 'policy') fs.writeFileSync(join(dir, 'report.json'), JSON.stringify({
        ok: false, errors: ['protected files: README.md'],
      }));
      if (phase === 'review') await new AsyncFunction('require', 'process', 'core',
        script('Validate the review verdict', 'script'))(require, { env }, core);
      await new AsyncFunction('require', 'process', 'core',
        script('Show why check refused the outputs', 'script'))(require, { env }, core);
      assert.ok(errors.some(text => text.startsWith('Refused by check: ')));
      assert.ok(outputs.reason.length > 0);
      const output = join(dir, 'step-output');
      const result = spawnSync('bash', ['-eo', 'pipefail', '-c', script('How the run ended', 'run')], {
        encoding: 'utf8', env: { ...env, RESULTS: 'success/failure/skipped', EXIT: '0', PR: '',
          REFUSAL: outputs.reason, WORKFLOW: 'test', RUN_URL: 'https://example.invalid/run', GITHUB_OUTPUT: output },
      });
      assert.equal(result.status, 0, result.stderr);
      assert.ok(fs.readFileSync(output, 'utf8').includes(`refused by check: ${outputs.reason}`));
    } finally {
      fs.rmSync(dir, { recursive: true, force: true });
    }
  });
}

test('conclusion excludes wildcard targets and retains output-repository routing', () => {
  const conclude = workflow.split('\n  conclude:')[1];
  assert.equal((conclude.match(/needs.policy.outputs.comment-target != '\*'/g) ?? []).length, 2);
  assert.match(conclude, /REPO: \$\{\{ inputs.output-repo \|\| github.repository \}\}/);
  assert.match(conclude, /\[\[ "\$TARGET" =~ \^\[0-9\]\+\$/);
  assert.doesNotMatch(conclude, /AGENTIC_JOB_APPLY_TOKEN/);
});
