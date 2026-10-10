// Exercise the actual workflow shell, without tokens or a forge.
const assert = require('node:assert/strict');
const { spawnSync } = require('node:child_process');
const { mkdtempSync, mkdirSync, readFileSync, writeFileSync, rmSync } = require('node:fs');
const { join } = require('node:path');
const { homedir } = require('node:os');
const { test } = require('node:test');

const workflow = readFileSync(join(__dirname, '../.github/workflows/apply.yml'), 'utf8');
const wrapper = readFileSync(join(__dirname, '../.github/workflows/agentic-job.yml'), 'utf8');

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

test('apply alone consumes the optional safe outputs PAT with job-token fallback', () => {
  const jobs = workflow.split('\njobs:\n')[1];
  const before = jobs.split('\n  apply:\n')[0];
  const [apply, after] = jobs.split('\n  apply:\n')[1].split('\n  conclude:\n');
  assert.ok(workflow.includes('      SAFE_OUTPUTS_PAT:\n'));
  assert.match(workflow.split('    secrets:\n')[1].split('    outputs:\n')[0], /required: false/);
  assert.ok(!before.includes('secrets.SAFE_OUTPUTS_PAT'));
  assert.ok(!after.includes('secrets.SAFE_OUTPUTS_PAT'));
  // The wrapper hands the PAT to the apply call, and to nothing else.
  const wrapperJobs = wrapper.split('\njobs:\n')[1].split(/\n  (?=[-\w]+:\n)/);
  assert.deepEqual(wrapperJobs.filter(job => job.includes('secrets.SAFE_OUTPUTS_PAT')).map(job => job.split(':')[0]), ['apply']);
  assert.match(wrapperJobs.find(job => job.startsWith('apply:')), /uses: \.\/\.github\/workflows\/apply\.yml\n/);
  const selection = 'secrets.SAFE_OUTPUTS_PAT';
  for (const key of ['token', 'github-token']) {
    assert.ok(apply.includes(`${key}: \${{ ${selection} || github.token }}`));
  }
  assert.ok(apply.includes('environment: ${{ fromJSON(inputs.policy).apply-environment }}'));
  assert.doesNotMatch(apply, /HAS_TOKEN|no passed .* secret/);
});

for (const pat of [undefined, '', 'test-pat']) {
  test(`credential expression wiring: PAT ${JSON.stringify(pat)}`, () => {
    const apply = workflow.split('\n  apply:\n')[1].split('\n  conclude:\n')[0];
    for (const key of ['token', 'github-token']) {
      const expression = apply.match(new RegExp(`^          ${key}: \\$\\{\\{ (.+) \\}\\}$`, 'm'))[1];
      // Evaluate the workflow's simple || selection; not an Actions environment-delivery test.
      assert.equal(Function('secrets', 'github', `return ${expression}`)(
        { SAFE_OUTPUTS_PAT: pat }, { token: 'job-token' }), pat || 'job-token');
      assert.ok(!expression.includes('environment'));
    }
  });
}

for (const filename of ['example', 'example-command', 'example-pull-request', 'example-schedule', 'review']) {
  test(`${filename} explicitly forwards the optional safe outputs PAT`, () => {
    const caller = readFileSync(join(__dirname, `../.github/workflows/${filename}.yml`), 'utf8');
    assert.ok(caller.includes('SAFE_OUTPUTS_PAT: ${{ secrets.SAFE_OUTPUTS_PAT }}'));
  });
}

test('every CI safe-output caller exercises job-token fallback without deployment secrets', () => {
  const ci = readFileSync(join(__dirname, '../.github/workflows/ci.yml'), 'utf8');
  const jobs = [...ci.matchAll(/^  ([-\w]+):\n([\s\S]*?)(?=^  [-\w]+:\n|$(?![\s\S]))/gm)];
  const callers = jobs.filter(([, , job]) =>
    /uses: \.\/\.github\/workflows\/(agentic-job|dispatch|example-compose)\.yml/.test(job));
  assert.deepEqual(callers.map(([, name]) => name), [
    'e2e-proposals', 'e2e-full', 'e2e-limit', 'e2e-event',
    'e2e-analysis', 'e2e-analysis-refused', 'e2e-dispatch', 'e2e-compose', 'e2e-review',
  ]);
  for (const [, name, caller] of callers) {
    assert.doesNotMatch(caller, /^    secrets:|\bsecrets\./m, name);
    assert.doesNotMatch(caller, /^      apply-environment:/m, name);
  }
  for (const name of ['e2e-analysis', 'e2e-analysis-refused', 'e2e-compose', 'e2e-review']) {
    const caller = callers.find(([, jobName]) => jobName === name)[2];
    assert.match(caller, /^      contents: read$/m, name);
    assert.doesNotMatch(caller, /^      contents: write$/m, name);
  }
});

test('issue actions keep checked repository and caps, not output-repo or event targets', () => {
  const root = mkdtempSync(join(homedir(), 'apply-test-'));
  try {
    writeFileSync(join(root, 'config.json'), JSON.stringify({
      close_issue: { max: 2 },
      add_labels: { max: 3, allowed: ['triage'], blocked: ['release'] },
    }));
    const result = command(root, 'bash', ['-euo', 'pipefail', '-c', step("Write the handlers' configuration")], {
      GH_AW_TMP: root, GITHUB_ENV: join(root, 'env'), REPO: 'owner/source',
      OUTPUT_REPO: 'owner/other', BASE: 'main', BRANCH_PREFIX: '',
      PARTIAL: '', TITLE_PREFIX: '', COMMENT_TARGET: '99', PULL_REQUEST: '{}',
    });
    assert.equal(result.status, 0, result.stderr);
    const config = JSON.parse(readFileSync(join(root, 'handler-config.json'), 'utf8'));
    assert.deepEqual(config.close_issue, {
      max: 2, 'target-repo': 'owner/source', target: '*', issue_intent: false, allow_body: false,
    });
    assert.deepEqual(config.add_labels, {
      max: 3, allowed: ['triage'], blocked: ['release'],
      'target-repo': 'owner/source', target: '*', issue_intent: false, create_if_missing: false,
    });
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

// The pieces take their settings from the policy call's JSON, which the
// caller composes: check and apply refuse it from another commit, and
// apply an empty ID, before anything is downloaded.
test('check and apply refuse a policy call of another commit, and apply an empty checked ID', () => {
  const checker = readFileSync(join(__dirname, '../.github/workflows/check.yml'), 'utf8');
  const sha = 'a'.repeat(40);
  const pinned = "The policy call is this workflow's commit";
  const jobs = [['check', checker.split('\n  check:\n')[1]],
    ['apply', workflow.split('\n  apply:\n')[1].split('\n  conclude:\n')[0]], ['conclude', workflow.split('\n  conclude:\n')[1]]];
  for (const [name, job] of jobs) {
    // The job's first step, before any checkout or download.
    assert.ok(job.split('    steps:\n')[1].startsWith('      # The pieces must be one commit'), name);
    assert.ok(job.indexOf(`- name: ${pinned}\n`) < job.search(/uses: actions\/(checkout|download-artifact)@|- name: How the run ended/), name);
    assert.match(job, /SOURCE_REPOSITORY: \$\{\{ fromJSON\(inputs.policy\).source-repository \}\}\n {10}SOURCE_SHA: \$\{\{ fromJSON\(inputs.policy\).source-sha \}\}\n {10}REPOSITORY: \$\{\{ job.workflow_repository \}\}\n {10}SHA: \$\{\{ job.workflow_sha \}\}\n/, name);
  }
  const guard = step(pinned);
  const body = source => source.split(`- name: ${pinned}\n`)[1].split(/\n      [-#]/)[0];
  assert.equal(body(checker), body(workflow));
  assert.equal(body(jobs[2][1]), body(workflow));
  for (const [repository, source, expected] of [
    ['o/r', sha, 0], ['o/other', sha, 1], ['o/r', 'b'.repeat(40), 1], ['o/r', '', 1], ['', '', 1],
  ]) {
    const result = command(homedir(), 'bash', ['-euo', 'pipefail', '-c', guard],
      { SOURCE_REPOSITORY: repository, SOURCE_SHA: source, REPOSITORY: 'o/r', SHA: repository === '' ? '' : sha });
    assert.equal(result.status, expected, `${repository} ${source}`);
  }
  const apply = jobs[1][1];
  assert.ok(apply.indexOf("- name: Require the checked outputs' artifact ID\n") < apply.indexOf('uses: actions/download-artifact@'));
  for (const [id, expected] of [['42', 0], ['', 1], ['0', 1], ['01', 1], ['1,2', 1], ['1\n2', 1]]) {
    const result = command(homedir(), 'bash', ['-euo', 'pipefail', '-c', step("Require the checked outputs' artifact ID")], { CHECKED: id });
    assert.equal(result.status, expected, JSON.stringify(id));
  }
});

// A caller that wires apply's checked ID from the agent job, or from
// another run, would have it apply what check never accepted.
test("apply takes only this run's check output, whatever the caller wired", () => {
  const apply = workflow.split('\n  apply:\n')[1].split('\n  conclude:\n')[0];
  const verify = "- name: The checked outputs are this run's check's\n";
  assert.ok(apply.indexOf("- name: Require the checked outputs' artifact ID\n") < apply.indexOf(verify));
  assert.ok(apply.indexOf(verify) < apply.indexOf('uses: actions/download-artifact@'));
  assert.ok(apply.indexOf(verify) < apply.indexOf('uses: actions/checkout@'));
  // Check uploads under the name apply looks for, and only on acceptance.
  const checker = readFileSync(join(__dirname, '../.github/workflows/check.yml'), 'utf8');
  assert.match(checker, /- id: upload\n {8}if: \$\{\{ success\(\) && [^\n]*\n(?:[^\n]*\n){2} {10}name: \$\{\{ fromJSON\(inputs.policy\).artifact-prefix \}\}checked-outputs\n/);
  assert.match(apply, /NAME: \$\{\{ fromJSON\(inputs.policy\).artifact-prefix \}\}checked-outputs\n/);
  const artifact = (name, run) => JSON.stringify({ id: 42, name, workflow_run: { id: run, head_sha: 'a'.repeat(40) } });
  // [what the read API says of artifact 42, status]
  for (const [answer, expected] of [
    [artifact('p-checked-outputs', 7), 0],
    // The agent job's proposals, or the outputs of another policy call.
    [artifact('p-safe-outputs', 7), 1], [artifact('checked-outputs', 7), 1], [artifact('q-checked-outputs', 7), 1],
    // Another run's.
    [artifact('p-checked-outputs', 8), 1], [artifact('p-checked-outputs', 77), 1], [JSON.stringify({ name: 'p-checked-outputs' }), 1],
    // Not found, or not readable without `actions: read`.
    ['', 1],
  ]) {
    const result = command(homedir(), 'bash', ['-eo', 'pipefail', '-c',
      'gh() { [ "$*" = "api repos/o/r/actions/artifacts/42" ] || return 9; [ -n "$ANSWER" ] && printf "%s" "$ANSWER"; }\n' + step("The checked outputs are this run's check's")],
      { CHECKED: '42', NAME: 'p-checked-outputs', GITHUB_REPOSITORY: 'o/r', GITHUB_RUN_ID: '7', ANSWER: answer });
    assert.equal(result.status, expected, `${answer}: ${result.stdout}${result.stderr}`);
  }
  // The callers whose apply job names its permissions grant it the read.
  const compose = readFileSync(join(__dirname, '../.github/workflows/example-compose.yml'), 'utf8');
  assert.match(compose.split('\n  apply:\n')[1], /^ {4}permissions:\n(?: {6}[-\w]+: \w+\n)*? {6}actions: read\n/m);
});

test('no failure in notify stops the run, and conclude still reports it', () => {
  const policy = readFileSync(join(__dirname, '../.github/workflows/policy.yml'), 'utf8');
  const notify = policy.split('\n  notify:\n')[1];
  // Nor does any other failure of notify's, its download or its target:
  // conclude then reports on the policy's item.
  assert.match(notify.split('    steps:\n')[0], /^ {4}continue-on-error: true$/m);
  assert.match(policy, /notified:\n[^\n]*\n {8}value: \$\{\{ jobs.notify.result != 'skipped' \}\}/);
  for (const name of ['An eyes reaction on what started the run', 'A comment that says the run started']) {
    const posted = notify.split(`- name: ${name}\n`)[1].split('\n      - ')[0];
    assert.match(posted, /^ {8}continue-on-error: true$/m, name);
  }
  // What names the target is still checked, and fails the job.
  assert.doesNotMatch(notify.split('- name: What to react to, and where to comment\n')[1].split('\n      - ')[0], /continue-on-error/);
  assert.match(notify, /^ {6}item: \$\{\{ steps.target.outputs.item \}\}$/m);
  assert.match(policy, /notify-item:\n[^\n]*\n {8}value: \$\{\{ jobs.notify.outputs.item \|\| jobs.policy.outputs.item \}\}/);
  const conclude = workflow.split('\n  conclude:\n')[1];
  assert.match(conclude, /- name: Edit the status comment\n {8}if: \$\{\{ fromJSON\(inputs.policy\).notify == 'comment' && fromJSON\(inputs.policy\).notified == 'true' \}\}/);
  assert.match(conclude, /fromJSON\(inputs.policy\).comment-target != '\*' && !\(fromJSON\(inputs.policy\).notify == 'comment' && fromJSON\(inputs.policy\).notified == 'true'\) \}\}/);
  // [comment id, item, status, the API call]
  for (const [id, item, expected, call] of [
    ['42', '7', 0, 'PATCH repos/o/r/issues/comments/42'], ['', '7', 0, 'POST repos/o/r/issues/7/comments'],
    ['', '', 1, ''], ['', '$(exit 9)', 1, ''], ['4 2', '7', 0, 'POST repos/o/r/issues/7/comments'],
  ]) {
    const result = command(__dirname, 'bash', ['-eo', 'pipefail', '-c', 'gh() { echo "$3 $4"; };\n' + step('Edit the status comment')],
      { ID: id, ITEM: item, TEXT: 'done', GITHUB_REPOSITORY: 'o/r' });
    assert.equal(result.status, expected, result.stdout + result.stderr);
    if (expected === 0) assert.equal(result.stdout.trim(), call);
  }
  // Reactions alone: a notify that failed before it found its target left
  // no id, and conclude reacts to nothing rather than failing.
  assert.match(conclude, /- name: A reaction that says how it ended\n {8}if: \$\{\{ fromJSON\(inputs.policy\).notify == 'reaction' && fromJSON\(inputs.policy\).notified == 'true' && fromJSON\(inputs.policy\).notify-id != '' \}\}/);
  // [kind, id, status, the API call]
  for (const [kind, id, expected, call] of [
    ['issue', '42', 0, 'POST repos/o/r/issues/42/reactions'], ['comment', '42', 0, 'POST repos/o/r/issues/comments/42/reactions'],
    ['review_comment', '42', 0, 'POST repos/o/r/pulls/comments/42/reactions'], ['issue', '$(exit 9)', 1, ''], ['issue', '4 2', 1, ''],
  ]) {
    const result = command(__dirname, 'bash', ['-eo', 'pipefail', '-c', 'gh() { echo "$3 $4"; };\n' + step('A reaction that says how it ended')],
      { KIND: kind, ID: id, REACTION: 'rocket', GITHUB_REPOSITORY: 'o/r' });
    assert.equal(result.status, expected, result.stdout + result.stderr);
    if (expected === 0) assert.equal(result.stdout.trim(), call);
  }
});

test('comment-only apply skips every repository step, including checkout', () => {
  const apply = workflow.split('\n  apply:\n')[1].split('\n  conclude:\n')[0];
  const gate = "if: ${{ fromJSON(inputs.check).outputs.has-patch == 'true' }}";
  assert.match(apply, new RegExp(`uses: actions/checkout@[^\\n]+\\n        ${gate.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}`));
  for (const name of ['Configure git', "Fetch the target's base branch, and bring a fork's up to it",
    'Apply the patch on its own base, and compare what changed with what was checked']) {
    assert.ok(apply.split(`- name: ${name}\n`)[1].startsWith(`        ${gate}\n`), name);
  }
  const checker = readFileSync(join(__dirname, '../.github/workflows/check.yml'), 'utf8');
  assert.match(checker, /has-patch: \$\{\{ steps.checked.outputs.has-patch \}\}/);
  assert.match(checker, /value: \$\{\{ jobs.check.outputs.has-patch \}\}/);
  const root = mkdtempSync(join(homedir(), 'apply-test-'));
  try {
    const report = join(root, 'report.json');
    writeFileSync(report, JSON.stringify({ patch: null }));
    // No .git exists and the output repository need not contain the topic base.
    const result = command(root, 'bash', ['-euo', 'pipefail', '-c', step('Apply the patch on its own base, and compare what changed with what was checked')], { REPORT: report });
    assert.equal(result.status, 0, result.stderr);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

for (const filename of ['full', 'limit', 'event']) {
  test(`scripted ${filename} comments inherit the caller's fixed destination`, () => {
    const setup = readFileSync(join(__dirname, `../.github/agentic-job/e2e/${filename}.sh`), 'utf8');
    const fixture = JSON.parse(setup.split("<<'JSON'\n")[1].split('\nJSON')[0]);
    // full's comment is appended by its GitHub probe, once the probe passed.
    const appended = [...setup.matchAll(/^printf '%s\\n' '(\{.*\})' >> "\$HOME\/out\/safe-outputs\.jsonl"$/gm)]
      .map(([, line]) => JSON.parse(line));
    const comments = fixture.filter(step => step.write?.path.endsWith('safe-outputs.jsonl'))
      .flatMap(step => step.write.content.trim().split('\n').map(line => JSON.parse(line)))
      .concat(appended)
      .filter(output => output.type === 'add_comment');
    assert.equal(comments.length, 1);
    assert.deepEqual(Object.keys(comments[0]).sort(), ['body', 'type']);
  });
}

test('scripted negative analysis reaches check with a misdirected comment', () => {
  const setup = readFileSync(join(__dirname, '../.github/agentic-job/e2e/misdirected.sh'), 'utf8');
  const fixture = JSON.parse(setup.split("<<'JSON'\n")[1].split('\nJSON')[0]);
  // Fake runs must exercise redaction before any hand-back may be uploaded.
  assert.ok(fixture.some(step => step.execute?.command.includes('gh%s_%s')));
  const outputs = fixture.filter(step => step.write?.path.endsWith('safe-outputs.jsonl'));
  assert.equal(outputs.length, 1);
  assert.deepEqual(JSON.parse(outputs[0].write.content), {
    type: 'add_comment', item_number: 65, body: 'This misdirected analysis must be refused.',
  });
  const outcome = fixture.find(step => step.write?.path.endsWith('outcome.json'));
  assert.equal(JSON.parse(outcome.write.content).stopped_early, null);
});

test('negative analysis verification refuses any posted misdirected comment', () => {
  const ci = readFileSync(join(__dirname, '../.github/workflows/ci.yml'), 'utf8');
  const filter = ci.split('- name: The refused comment was not posted on either item\n')[1]
    .split("--arg run \"$RUN\" '\n")[1].split("'\n")[0].trim();
  const hostile = { body: 'This misdirected analysis must be refused. actions/runs/123' };
  for (const [comments, expected] of [
    [[], 0],
    [[{ body: 'unrelated comment' }], 0],
    [[hostile], 1],
    [[{ body: 'unrelated comment' }, hostile], 1],
  ]) {
    const result = spawnSync('jq', ['-se', '--arg', 'run', 'actions/runs/123', filter], {
      encoding: 'utf8', input: JSON.stringify(comments),
    });
    assert.equal(result.status, expected, result.stderr);
  }
});

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

function script(name) {
  const section = workflow.split(`- name: ${name}\n`)[1].split(/\n      - /)[0];
  return section.split('script: |\n')[1].split('\n')
    .filter(line => line.startsWith('            ')).map(line => line.slice(12)).join('\n');
}

const AsyncFunction = Object.getPrototypeOf(async function () {}).constructor;

// Runs the guard against a forge holding `posted`, by whoever posted it,
// with the job token or, with `pat`, a PAT that /user names as `me`.
async function leaveOutApplied(items, posted, { pat = false, me = null, patch = 'diff\n', target = '64', run = '7', prefix = 'e2e/', artifacts = '' } = {}) {
  const dir = mkdtempSync(join(homedir(), 'apply-test-'));
  try {
    writeFileSync(join(dir, 'agent_output.json'), JSON.stringify({ items, errors: [] }));
    const file = `aw-agent-run-${run}.patch`;
    writeFileSync(join(dir, 'report.json'), JSON.stringify({ patch: patch === null ? null : { file } }));
    if (patch !== null) writeFileSync(join(dir, file), patch);
    const outputs = {}, summary = [], warnings = [];
    const core = { setOutput: (key, value) => { outputs[key] = value; }, warning: text => { warnings.push(text); },
      summary: { addHeading: text => { summary.push(text); return core.summary; },
        addList: list => { summary.push(...list); return core.summary; }, write: async () => {} } };
    const queries = [];
    const github = {
      paginate: async (method, params) => method(params),
      rest: {
        users: { getAuthenticated: async () => { if (me === null) throw new Error('403'); return { data: { login: me } }; } },
        issues: { listComments: params => { queries.push(['comments', params.issue_number]); return posted.filter(p => p.on === params.issue_number); } },
        pulls: { list: params => { queries.push(['pulls', params.head]); return posted.filter(p => p.head === params.head); } },
        search: { issuesAndPullRequests: async params => { queries.push(['search', params.q]);
          return { data: { items: posted.filter(p => p.issue && params.q.includes(p.body.match(/<!-- (.*) -->/)?.[1])) } }; } },
      },
    };
    const env = { GH_AW_TMP: dir, REPORT: join(dir, 'report.json'), OUTPUT_REPO: 'owner/repo', GITHUB_RUN_ID: run,
      COMMENT_TARGET: target, BRANCH_PREFIX: prefix, ARTIFACT_PREFIX: artifacts, RUNNER_TEMP: '/runner', TOKEN_IS_PAT: String(pat) };
    // gh-aw's normalize_branch_name.cjs, as far as these prefixes go.
    const normalizeBranchName = name => name.replace(/[^a-zA-Z0-9\-_/.]+/g, '-');
    const fakeRequire = path => path === '/runner/gh-aw/actions/normalize_branch_name.cjs' ? { normalizeBranchName } : require(path);
    await new AsyncFunction('require', 'process', 'core', 'github', script('Leave out what an earlier attempt applied'))(
      fakeRequire, { env }, core, github);
    const kept = JSON.parse(readFileSync(join(dir, 'agent_output.json'), 'utf8')).items;
    const skipped = JSON.parse(readFileSync(join(dir, 'skipped.json'), 'utf8'));
    return { kept, skipped, outputs, summary, queries, warnings };
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

// What the handlers send, after the request hook hides the name.
function sent(body) {
  const line = workflow.split('\n').find(text => text.includes('options.body = options.body.replace('));
  const options = { body };
  new Function('options', line)(options);
  return options.body;
}

const comment = (body = 'first') => ({ type: 'add_comment', body });
// The name the guard put in a body it kept: its last line.
const nameIn = item => item.body.split('\n').at(-1);
const pull = { type: 'create_pull_request', title: 't', body: 'change', branch: 'agent-run-7' };

test('a posted body hides the name the guard put last, and only that name', () => {
  const name = 'agentic-job-applied: 7/0/0123456789abcdef';
  const other = 'agentic-job-applied: 7/1/fedcba9876543210';
  assert.equal(sent(`hello\n\n${name}`), `hello\n\n<!-- ${name} -->`);
  // A handler's footer may follow the name.
  assert.equal(sent(`hello\n\n${name}\n\n> footer`), `hello\n\n<!-- ${name} -->\n\n> footer`);
  // A line the handlers decoded into a name before the guard's stays as it is.
  assert.equal(sent(`${other}\n\n${name}`), `${other}\n\n<!-- ${name} -->`);
  for (const body of ['hello', `say ${name}`, 'agentic-job-applied: 7/0/0123', 'agentic-job-applied: 7/0/0123456789ABCDEF']) {
    assert.equal(sent(body), body);
  }
});

test('nothing applied yet: every created thing is named in its body, the rest pass as they are', async () => {
  const items = [comment(), pull, { type: 'create_issue', title: 'i', body: 'b' },
    { type: 'close_issue', item_number: 3, body: 'done' }, { type: 'add_labels', labels: ['x'] },
    { type: 'update_project', project: 'p', fields: { Status: 'Done' } }, { type: 'noop', message: 'n' }];
  const { kept, skipped, summary, queries } = await leaveOutApplied(items, []);
  assert.deepEqual(skipped, []);
  assert.deepEqual(summary, []);
  assert.equal(kept.length, items.length);
  for (const [index, item] of kept.entries()) {
    if (index < 3) {
      assert.match(item.body, new RegExp(`^${items[index].body}\n\nagentic-job-applied: 7/${index}/[0-9a-f]{16}$`));
      assert.deepEqual({ ...item, body: items[index].body }, items[index]);
    } else {
      // Closing, labelling and setting a field again leave the forge as it was.
      assert.deepEqual(item, items[index]);
    }
  }
  assert.deepEqual(queries.map(([kind]) => kind), ['comments', 'pulls', 'search']);
  assert.deepEqual(queries[1], ['pulls', 'owner:e2e/agent-run-7']);
});

test('the name is stable across attempts and differs by run, call, place and content', async () => {
  const name = async (items, run, artifacts = '') => nameIn((await leaveOutApplied(items, [], { run, artifacts })).kept[0]);
  assert.equal(await name([comment()], '7'), await name([comment()], '7'));
  assert.equal(await name([comment()], '7', 'dispatch-triage-'), await name([comment()], '7', 'dispatch-triage-'));
  assert.notEqual(await name([comment()], '7'), await name([comment()], '8'));
  assert.notEqual(await name([comment()], '7', 'dispatch-triage-'), await name([comment()], '7', 'dispatch-research-'));
  assert.notEqual(await name([comment()], '7'), await name([comment()], '7', 'full-'));
  assert.notEqual(await name([comment()], '7'), await name([comment('other')], '7'));
  const both = (await leaveOutApplied([comment(), comment()], [])).kept.map(nameIn);
  assert.notEqual(both[0], both[1]);
});

// CI run 38018010779: the triage and research calls of one run post the
// same comment on the same issue; the second was taken for applied.
test('another call of the same run posting the same comment does not leave it out', async () => {
  const shipped = readFileSync(join(__dirname, 'dispatch-comment.sh'), 'utf8');
  const output = JSON.parse(JSON.parse(shipped.split("<<'JSON'\n")[1].split('\nJSON')[0])
    .find(step => step.write?.path.endsWith('safe-outputs.jsonl')).write.content);
  const triage = (await leaveOutApplied([output], [], { artifacts: 'dispatch-triage-' })).kept[0];
  const posted = [{ on: 64, user: actions, body: sent(triage.body), html_url: 'u' }];
  const research = await leaveOutApplied([output], posted, { artifacts: 'dispatch-research-' });
  assert.deepEqual(research.skipped, []);
  assert.equal(research.kept.length, 1);
  const again = await leaveOutApplied([output], posted, { artifacts: 'dispatch-triage-' });
  assert.equal(again.kept.length, 0);
});

// CI run 38018010779 too: a verdict must stay the first line of what is posted.
for (const body of [
  'VERDICT: APPROVE\nREASON: Scripted dispatch tests wiring.\nReviewed SHA: 0123',
  '**Partial work.** The run was stopped at its timeout before the task was done; this is what it handed back.\n\nrest',
]) {
  test(`the posted body keeps its first line: ${body.split('\n')[0].slice(0, 20)}`, async () => {
    const [kept] = (await leaveOutApplied([comment(body)], [])).kept;
    const posted = sent(`${kept.body}\n\n> footer`);
    assert.ok(posted.startsWith(`${body}\n\n<!-- agentic-job-applied: 7/0/`), posted);
  });
}

const actions = { login: 'github-actions[bot]', type: 'Bot' };
const app = { login: 'unrelated-app[bot]', type: 'Bot' };
const maintainer = { login: 'maintainer', type: 'User' };
const mallory = { login: 'mallory', type: 'User' };

// Who posted the named comment, and with which token apply runs: the job
// token (`pat` false), a PAT /user names (`me`), or a token in
// SAFE_OUTPUTS_PAT that /user does not name, as an app's.
for (const [name, user, token, left] of [
  ['the job token, with the job token', actions, {}, true],
  ['an unrelated bot, with the job token', app, {}, false],
  ['a commenter, with the job token', mallory, {}, false],
  ['a user called github-actions, with the job token', { login: 'github-actions', type: 'User' }, {}, false],
  ['the PAT, with the PAT', maintainer, { pat: true, me: 'maintainer' }, true],
  ['an unrelated bot, with a PAT', app, { pat: true, me: 'maintainer' }, false],
  ['the job token, with a PAT', actions, { pat: true, me: 'maintainer' }, false],
  ['a commenter, with a PAT', mallory, { pat: true, me: 'maintainer' }, false],
  ['an unrelated bot, with an app token', app, { pat: true }, false],
  ['the job token, with an app token', actions, { pat: true }, false],
]) {
  test(`a comment named by ${name} ${left ? 'is left out' : 'does not count'}`, async () => {
    const first = (await leaveOutApplied([comment()], [], token)).kept[0];
    const posted = [{ on: 64, user, body: sent(first.body), html_url: 'https://forge/c/1' }];
    const { kept, skipped, summary } = await leaveOutApplied([comment()], posted, token);
    assert.equal(kept.length, left ? 0 : 1);
    assert.equal(skipped.length, left ? 1 : 0);
    if (left) assert.deepEqual(summary, ['Already applied by an earlier attempt, so left out', 'add_comment: https://forge/c/1']);
  });
}

test('a PAT that /user does not name finds nothing applied, and says so', async () => {
  const { warnings } = await leaveOutApplied([comment()], [], { pat: true });
  assert.equal(warnings.length, 1);
  assert.match(warnings[0], /nothing posted before counts as applied/);
  assert.deepEqual((await leaveOutApplied([comment()], [])).warnings, []);
  assert.deepEqual((await leaveOutApplied([comment()], [], { pat: true, me: 'maintainer' })).warnings, []);
  // Whom the link to the issue is looked for by, after the guard.
  for (const [token, poster] of [[{}, 'github-actions[bot]'], [{ pat: true, me: 'maintainer' }, 'maintainer'], [{ pat: true }, '']]) {
    assert.equal((await leaveOutApplied([comment()], [], token)).outputs.poster, poster);
  }
});


test('a name the handlers decoded into a posted body does not leave another request out', async () => {
  const items = [comment('one'), comment('two')];
  const first = (await leaveOutApplied(items, [])).kept;
  // What check saw as entities or invisible characters, decoded by the
  // handlers' sanitizer into the second request's name, hidden or not,
  // before the first's.
  for (const decoded of [`\`\`\`\n<!-- ${nameIn(first[1])} -->\n\`\`\``, nameIn(first[1])]) {
    const forged = `one\n${decoded}\n\n${nameIn(first[0])}`;
    const posted = [{ on: 64, user: actions, body: sent(forged), html_url: 'u' }];
    const { kept, skipped } = await leaveOutApplied(items, posted);
    assert.equal(skipped.length, 1);
    assert.deepEqual(kept, first.slice(1));
  }
});

test('a re-run after a half-applied attempt applies only what was not', async () => {
  const items = [comment('one'), comment('two'), { type: 'add_labels', labels: ['x'] }];
  const first = (await leaveOutApplied(items, [])).kept;
  // The first attempt posted the first comment and failed before the second.
  const posted = [{ on: 64, user: actions, body: sent(first[0].body), html_url: 'u' }];
  const { kept, skipped } = await leaveOutApplied(items, posted);
  assert.deepEqual(skipped.map(item => item.name), [nameIn(first[0])]);
  assert.deepEqual(kept, first.slice(1));
});

test('a comment is looked for where it goes, and not where none can be found', async () => {
  const item = { ...comment(), item_number: 12 };
  assert.deepEqual((await leaveOutApplied([item], [], { target: 'triggering' })).queries, [['comments', 12]]);
  assert.deepEqual((await leaveOutApplied([item], [], { target: '64' })).queries, [['comments', 64]]);
  const nowhere = await leaveOutApplied([comment()], [], { target: '*' });
  assert.deepEqual(nowhere.queries, []);
  assert.equal(nowhere.kept.length, 1);
});

test("the pull request is looked for from the branch gh-aw names, prefix normalized", async () => {
  const { queries } = await leaveOutApplied([pull], [], { prefix: 'bot run/' });
  assert.deepEqual(queries, [['pulls', 'owner:bot-run/agent-run-7']]);
  assert.deepEqual((await leaveOutApplied([pull], [], { prefix: '' })).queries, [['pulls', 'owner:agent-run-7']]);
});

test('closing an issue again posts nothing: its handler is told to drop any body', () => {
  assert.match(step("Write the handlers' configuration"),
    /\(select\(has\("close_issue"\)\)\.close_issue\) \+= \{[^}]*allow_body: false\}/);
});

for (const [state, merged, left] of [['open', null, true], ['closed', '2026-10-01T00:00:00Z', true], ['closed', null, false]]) {
  test(`a pull request ${state}${merged ? ' and merged' : ''} from the run's branch ${left ? 'is left out' : 'is not'}`, async () => {
    const first = (await leaveOutApplied([pull], [])).kept[0];
    const posted = [{ head: 'owner:e2e/agent-run-7', state, merged_at: merged, number: 39,
      user: actions, body: sent(first.body), html_url: 'https://forge/pull/39' }];
    const { kept, skipped, outputs } = await leaveOutApplied([pull], posted);
    assert.equal(kept.length, left ? 0 : 1);
    assert.deepEqual(skipped.map(item => item.url), left ? ['https://forge/pull/39'] : []);
    assert.deepEqual(outputs, { poster: 'github-actions[bot]',
      ...left ? { 'pull-request-number': 39, 'pull-request-url': 'https://forge/pull/39' } : {} });
  });
}

test('an issue an earlier attempt opened is found by its name and left out', async () => {
  const issue = { type: 'create_issue', title: 'i', body: 'b' };
  const first = (await leaveOutApplied([issue], [])).kept[0];
  const posted = [{ issue: true, user: actions, body: sent(first.body), html_url: 'https://forge/issues/5' }];
  const { kept, skipped, queries } = await leaveOutApplied([issue], posted);
  assert.equal(kept.length, 0);
  assert.equal(skipped[0].url, 'https://forge/issues/5');
  assert.match(queries[0][1], /^repo:owner\/repo is:issue in:body "agentic-job-applied: 7\/0\/[0-9a-f]{16}"$/);
});

test("a pull request's name holds the checked patch; a comment's does not", async () => {
  const name = async (items, patch) => nameIn((await leaveOutApplied(items, [], { patch })).kept[0]);
  assert.equal(await name([pull], 'diff\n'), await name([pull], 'diff\n'));
  assert.notEqual(await name([pull], 'diff\n'), await name([pull], 'other\n'));
  assert.equal(await name([comment()], 'diff\n'), await name([comment()], 'other\n'));
});

for (const [state, merged] of [['open', null], ['closed', '2026-10-01T00:00:00Z']]) {
  test(`a different patch with the same request is not taken for the ${state} pull request`, async () => {
    const first = (await leaveOutApplied([pull], [])).kept[0];
    const posted = [{ head: 'owner:e2e/agent-run-7', state, merged_at: merged, number: 39,
      user: actions, body: sent(first.body), html_url: 'https://forge/pull/39' }];
    const { kept, skipped, outputs } = await leaveOutApplied([pull], posted, { patch: 'other\n' });
    // Applied as new: the handler then stops at the branch already there.
    assert.equal(kept.length, 1);
    assert.deepEqual(skipped, []);
    assert.deepEqual(outputs, { poster: 'github-actions[bot]' });
  });
}

test('a pull request without a checked patch stops the guard', async () => {
  await assert.rejects(leaveOutApplied([pull], [], { patch: null }), /check accepted no patch/);
  assert.equal((await leaveOutApplied([comment()], [], { patch: null })).kept.length, 1);
});

function refs(items, issue = '64') {
  const dir = mkdtempSync(join(homedir(), 'refs-test-'));
  try {
    writeFileSync(join(dir, 'agent_output.json'), JSON.stringify({ items, errors: [] }));
    const result = command(dir, 'bash', ['-e', '-o', 'pipefail', '-c', step('Say which issue a pull request is for')],
      { GH_AW_TMP: dir, RUNNER_TEMP: dir, OUTPUT_REPO: 'owner/repo', ISSUE: issue });
    assert.equal(result.status, 0, result.stderr);
    return JSON.parse(readFileSync(join(dir, 'agent_output.json'), 'utf8')).items;
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

test('a pull request refers to the issue the caller named, and closes nothing', () => {
  const [opened, commented] = refs([pull, comment()]);
  assert.equal(opened.body, 'change\n\nRefs owner/repo#64');
  assert.deepEqual(commented, comment());
  assert.equal(refs([{ ...pull, body: undefined }])[0].body, '\n\nRefs owner/repo#64');
  // GitHub's closing keywords, none of which the line starts with.
  assert.doesNotMatch(opened.body.split('\n').at(-1), /^(close[sd]?|fix(e[sd])?|resolve[sd]?)\b/i);
  const steps = workflow.split('\n      - name: ').map(text => text.split('\n')[0]);
  assert.ok(steps.indexOf('Say which issue a pull request is for') < steps.indexOf('Leave out what an earlier attempt applied'));
  assert.match(workflow.split('- name: Say which issue a pull request is for\n')[1].split('\n')[0],
    /if: \$\{\{ fromJSON\(inputs\.policy\)\.issue != '' \}\}/);
});

// Runs the step after upload against a forge holding `posted` on the issue.
async function result({ made = [], skipped = [], pr = null, issue = '64', posted = [], poster = 'github-actions[bot]', artifacts = '' } = {}) {
  const dir = mkdtempSync(join(homedir(), 'result-test-'));
  try {
    writeFileSync(join(dir, 'applied.json'), JSON.stringify({ made, already_applied: skipped, pull_request: pr }));
    const outputs = {}, summary = [], queries = [], created = [];
    const core = { setOutput: (key, value) => { outputs[key] = value; },
      summary: { addHeading: text => { summary.push(text); return core.summary; },
        addLink: (text, href) => { summary.push(href); return core.summary; }, write: async () => {} } };
    const github = {
      paginate: async (method, params) => method(params),
      rest: { issues: {
        listComments: params => { queries.push(params); return posted; },
        createComment: async params => { created.push(params); },
      } },
    };
    const env = { GH_AW_TMP: dir, OUTPUT_REPO: 'owner/repo', ISSUE: issue, POSTER: poster, ARTIFACT_PREFIX: artifacts,
      GITHUB_RUN_ID: '7', RUN_URL: 'https://github.com/caller/repo/actions/runs/7' };
    await new AsyncFunction('require', 'process', 'core', 'github', script('Say where what the run made is'))(
      require, { env }, core, github);
    return { outputs, summary, queries, created };
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

const opened = { number: 39, url: 'https://github.com/owner/repo/pull/39' };
const on = item => ({ type: 'add_comment', url: `https://github.com/owner/repo/issues/${item}#issuecomment-1` });

for (const [name, change, url, linked] of [
  ['a pull request', { pr: opened }, opened.url, true],
  ['a pull request an earlier attempt opened', { pr: opened, skipped: [{ type: 'create_pull_request', url: opened.url }] }, opened.url, true],
  ['a comment on the issue', { made: [on(64)] }, on(64).url, false],
  ['a comment an earlier attempt posted on the issue', { skipped: [on(64)] }, on(64).url, false],
  ['a comment on a pull request of that number', { made: [{ type: 'add_comment', url: 'https://github.com/Owner/Repo/pull/64#issuecomment-2' }] },
    'https://github.com/Owner/Repo/pull/64#issuecomment-2', false],
  ['a comment elsewhere', { made: [on(12)] }, on(12).url, true],
  ['a pull request beside a comment on the issue', { pr: opened, made: [on(64)] }, opened.url, true],
  // An earlier attempt's comment stays the result when this one opens an issue.
  ['a comment an earlier attempt posted elsewhere, then an issue', { skipped: [on(12)],
    made: [{ type: 'create_issue', url: 'https://github.com/owner/repo/issues/5' }] }, on(12).url, true],
  ['a comment on issue 640', { made: [on(640)] }, on(640).url, true],
  ['an issue', { made: [{ type: 'create_issue', url: 'https://github.com/owner/repo/issues/5' }] }, 'https://github.com/owner/repo/issues/5', true],
  ['nothing', { made: [{ type: 'noop' }] }, '', false],
  ['a pull request, for no issue', { pr: opened, issue: '' }, opened.url, false],
]) {
  test(`the result of ${name} is ${url || 'nothing'}, ${linked ? 'linked from' : 'not linked from'} the issue`, async () => {
    const { outputs, summary, queries, created } = await result(change);
    assert.equal(outputs['result-url'], url);
    assert.deepEqual(summary, url ? ['What the run made', url] : []);
    assert.equal(created.length, linked ? 1 : 0);
    assert.equal(queries.length, linked ? 1 : 0);
    if (linked) {
      assert.deepEqual({ ...created[0], body: undefined }, { owner: 'owner', repo: 'repo', issue_number: 64, body: undefined });
      assert.match(created[0].body, new RegExp(`^The \\[run\\]\\(https://github\\.com/caller/repo/actions/runs/7\\) for this made ${url.replace(/[.?]/g, '\\$&')}\n\n<!-- agentic-job-linked: 7/[0-9a-f]{16} -->$`));
    }
  });
}

test('the issue is linked to what a run made once per run and call, by the token that posts', async () => {
  const [first] = (await result({ pr: opened })).created;
  const again = posted => result({ pr: opened, posted });
  assert.equal((await again([{ user: actions, body: first.body }])).created.length, 0);
  // Another poster's copy is not this token's link, and neither is another call's.
  assert.equal((await again([{ user: mallory, body: first.body }])).created.length, 1);
  assert.equal((await result({ pr: opened, posted: [{ user: maintainer, body: first.body }], poster: '' })).created.length, 1);
  assert.equal((await result({ pr: opened, posted: [{ user: maintainer, body: first.body }], poster: 'maintainer' })).created.length, 0);
  // A re-run whose result is another is not linked again.
  const other = { number: 40, url: 'https://github.com/owner/repo/pull/40' };
  assert.equal((await result({ pr: other, posted: [{ user: actions, body: first.body }] })).created.length, 0);
  assert.equal((await result({ pr: opened, posted: [{ user: actions, body: first.body }], artifacts: 'dispatch-implement-' })).created.length, 1);
});
