// Test the actual mode gate and routing without forge credentials.
const assert = require('node:assert/strict');
const { readFileSync, readdirSync } = require('node:fs');
const { join } = require('node:path');
const { spawnSync } = require('node:child_process');
const { test } = require('node:test');

const workflow = readFileSync(join(__dirname, '../.github/workflows/agentic-job.yml'), 'utf8');
const gate = workflow.split('- name: Check the proposals mode\n')[1]
  .split('        run: |\n')[1].split('      #')[0]
  .split('\n').filter(line => line.startsWith('          ')).map(line => line.slice(10)).join('\n');

function checkMode(change) {
  return spawnSync('bash', ['-e', '-o', 'pipefail', '-c', gate], {
    encoding: 'utf8', env: { ...process.env, PROPOSALS: '', EVENT: 'false',
      REVIEW: 'false', PARTIAL: 'false', TASK: '', ...change },
  });
}

test('mode gate accepts derived tasks and refuses incompatible proposals', () => {
  for (const [name, change, expected] of [
    ['proposals', { PROPOSALS: 'proposals' }, 0],
    ['proposals with event', { PROPOSALS: 'proposals', EVENT: 'true' }, 1],
    ['proposals with review', { PROPOSALS: 'proposals', REVIEW: 'true' }, 1],
    ['proposals with partial apply', { PROPOSALS: 'proposals', PARTIAL: 'true' }, 1],
    ['direct task', { TASK: 'run' }, 0],
    ['missing direct task', {}, 1],
    ['partial direct task', { TASK: 'run', PARTIAL: 'true' }, 0],
    ['partial without task', { PARTIAL: 'true' }, 1],
    ['event-derived task', { EVENT: 'true' }, 0],
    ['review-derived task', { REVIEW: 'true' }, 0],
    ['event review', { EVENT: 'true', REVIEW: 'true' }, 0],
    ['artifact name is data', { PROPOSALS: '$(exit 99)' }, 0],
  ]) {
    const result = checkMode(change);
    assert.equal(result.status, expected, `${name}: ${result.stdout}${result.stderr}`);
  }
});

test('every checked-in caller passes the mode gate', () => {
  const directory = join(__dirname, '../.github/workflows');
  const inputs = { 'proposals-artifact': 'PROPOSALS', event: 'EVENT', review: 'REVIEW',
    'apply-partial': 'PARTIAL', task: 'TASK' };
  let callers = 0;
  for (const file of readdirSync(directory).filter(file => file.endsWith('.yml'))) {
    const source = readFileSync(join(directory, file), 'utf8');
    // Local reusable-workflow calls are job-level, with inputs indented six spaces.
    for (const block of source.split(/^  [\w-]+:\s*$/m).slice(1)) {
      if (!/^    uses: \.\/\.github\/workflows\/agentic-job\.yml\s*$/m.test(block)) continue;
      const change = {};
      for (const [input, variable] of Object.entries(inputs)) {
        const match = block.match(new RegExp(`^      ${input}: *(.*)$`, 'm'));
        if (!match) continue;
        const value = match[1].trim();
        // Folded task prose and dispatch expressions represent supplied tasks;
        // the mode check does not interpret their contents.
        change[variable] = value === "''" || value === '""' ? '' : value;
      }
      const result = checkMode(change);
      assert.equal(result.status, 0, `${file}: ${result.stdout}${result.stderr}`);
      callers++;
    }
  }
  assert.ok(callers >= 8, `expected the example, review and CI callers, found ${callers}`);
});

test('proposals route retains check and token separation', () => {
  const check = workflow.split('\n  check:\n')[1].split('\n  apply:\n')[0];
  assert.match(check, /needs.policy.result == 'success'/);
  assert.match(check, /permissions:\n      contents: read/);
  assert.match(check, /name: \$\{\{ inputs.proposals-artifact \}\}/);
  assert.match(check, /artifact-ids: \$\{\{ needs.agent.outputs.safe-outputs-artifact-id \}\}/);
  assert.match(check, /agentic-job" check --policy/);
  assert.doesNotMatch(check, /secrets\./);
  const apply = workflow.split('\n  apply:\n')[1];
  assert.match(apply, /needs.check.result == 'success' && \(inputs.proposals-artifact != ''/);
  assert.match(apply, /inputs.proposals-artifact == '' && needs.agent.outputs.exit != '0'/);
  assert.match(apply.split('- name: Write applied.json')[1],
    /EXIT: \$\{\{ inputs.proposals-artifact != '' && '0' \|\| needs.agent.outputs.exit \}\}/);
  for (const job of ['activate', 'agent']) {
    assert.match(workflow.split(`\n  ${job}:\n`)[1].split('    steps:')[0],
      /inputs.proposals-artifact == ''/);
  }
});
