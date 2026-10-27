'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const os = require('node:os');
const { execFileSync } = require('node:child_process');

const root = path.join(__dirname, '..');
const workflow = fs.readFileSync(path.join(root, '.github/workflows/dispatch.yml'), 'utf8');
const bounds = fs.readFileSync(path.join(root, '.github/agentic-job/dispatch.toml'), 'utf8');

// status-comment only chooses which of the status credential's own
// comments on the item `agentic-job status` edits (docs/workflow.md says what a
// wrong id replaces); it grants no new permission.
test('dispatch exposes only task, repo, item, item-repo, kind and status-comment', () => {
  const inputs = workflow.split('  workflow_dispatch:\n')[1].split('\npermissions:')[0];
  assert.deepEqual([...inputs.matchAll(/^      ([-\w]+):$/gm)].map(m => m[1]),
    ['task', 'repo', 'item', 'item-repo', 'kind', 'status-comment']);
});

test('caller fixes capabilities and token boundary', () => {
  assert.match(workflow, /uses: \.\/\.github\/workflows\/agentic-job.yml/);
  assert.match(workflow, /needs: target/);
  // (each profile's routing, evaluated as the runner would)
  const routed = (input, kind, agent = 'claude') => {
    const expression = workflow.match(new RegExp(`^      ${input}: \\$\\{\\{ (.+) \\}\\}$`, 'm'))?.[1];
    assert.ok(expression, input);
    return Function('inputs', 'needs', `return ${expression}`)(
      { kind, item: '42' }, { target: { outputs: { agent } } });
  };
  for (const [kind, run, outputs, max, review, push, fake] of [
    ['implement', 'branch', 'create_pull_request,noop,missing_tool,missing_data', '3', '', '', 'implement'],
    ['fix', 'branch', 'push_to_pull_request_branch,noop,missing_tool,missing_data', '3', '', '42', 'implement'],
    ['review', 'analysis', 'add_comment,noop', '1', '42', '', 'review'],
    // Up to the bounds' max_outputs: the closes and the comment.
    ['triage', 'analysis', 'close_issue,add_comment,noop,missing_tool,missing_data', 'max', '', '', 'comment'],
    ['research', 'analysis', 'add_comment,noop,missing_tool,missing_data', '3', '', '', 'comment'],
  ]) {
    assert.deepEqual(['kind', 'outputs', 'max-outputs', 'review-item', 'push-item', 'fake-profile'].map(input => routed(input, kind, 'fake')),
      [run, outputs, max, review, push, fake], kind);
    assert.equal(routed('fake-profile', kind), '', kind);
  }
  for (const line of ['allow: .github/agentic-job/dispatch.toml',
    'comment-target: ${{ inputs.item }}', 'issue: ${{ inputs.item }}', 'output-repo: ${{ inputs.repo }}', 'item-repo: ${{ inputs.item-repo }}',
    'apply-environment: ${{ !inputs.scripted && vars.APPLY_ENVIRONMENT || \'\' }}', 'apply-partial: false', "notify: ${{ (inputs.kind == 'review' || inputs.scripted) && 'none' || 'comment' }}",
    'model: ${{ needs.target.outputs.agent != \'fake\' && vars.AGENT_MODEL || \'\' }}', 'inference-url: ${{ needs.target.outputs.agent != \'fake\' && vars.INFERENCE_URL || \'\' }}',
    'inference-audience: ${{ needs.target.outputs.agent != \'fake\' && vars.INFERENCE_AUDIENCE || \'\' }}',
    'inference-register: ${{ needs.target.outputs.agent != \'fake\' && vars.INFERENCE_REGISTER || \'github-oidc\' }}',
    'inference-provider: ${{ needs.target.outputs.agent != \'fake\' && vars.INFERENCE_PROVIDER || \'\' }}']) {
    assert.ok(workflow.includes(line), line);
  }
  assert.doesNotMatch(workflow, /^\s+runner:/m);
  assert.doesNotMatch(workflow, /secrets: inherit/);
  const target = workflow.split('\n  target:\n')[1].split('\n  run:\n')[0];
  assert.doesNotMatch(target, /secrets(?:[.:]|\[)/);
  const forwarded = workflow.split('\n  run:\n')[1].split('    secrets:\n')[1].split('    permissions:\n')[0];
  const names = ['SAFE_OUTPUTS_PAT'];
  assert.deepEqual([...forwarded.matchAll(/^      ([-\w]+):/gm)].map(m => m[1]), names);
  // Declared so that callers that pass it still parse, but not forwarded:
  // agentic-job.yml drops it and refuses `api-key`.
  assert.ok(workflow.includes('      INFERENCE_API_KEY:\n        required: false'));
  assert.doesNotMatch(workflow, /secrets\.INFERENCE_API_KEY/);
  for (const name of names) {
    assert.ok(workflow.includes(`      ${name}:\n        required: false`), name);
    const line = `${name}: \${{ !inputs.scripted && secrets.${name} || '' }}`;
    assert.ok(forwarded.includes(line), name);
    const expression = line.match(/\$\{\{ (.+) \}\}/)[1];
    for (const scripted of [false, true]) {
      for (const value of ['', 'test-value']) {
        assert.equal(Function('inputs', 'secrets', `return ${expression}`)(
          { scripted }, { [name]: value }), scripted ? '' : value);
      }
    }
  }
  assert.match(bounds, /^repos = \["cgwalters-forge\/agentic-job"\]$/m);
  assert.match(bounds, /^max_outputs = 32$/m);
  for (const type of ['create_pull_request', 'add_comment', 'noop', 'missing_tool', 'missing_data']) {
    assert.ok(bounds.includes(`${type} = { max = 1 }`), type);
  }
  assert.match(bounds, /^close_issue = \{ max = 30 \}$/m);
  assert.doesNotMatch(bounds, /create_issue/);
});

test('agent configuration variables reach only opencode and vibe', () => {
  const review = fs.readFileSync(path.join(root, '.github/workflows/review.yml'), 'utf8');
  for (const [text, context] of [
    [workflow, agent => ({ needs: { target: { outputs: { agent } } } })],
    [review, agent => ({ vars: { AGENT: agent } })],
  ]) {
    for (const [input, variable] of [
      ['agent-config-repo', 'AGENT_CONFIG_REPO'], ['agent-config-path', 'AGENT_CONFIG_PATH'],
    ]) {
      const expression = text.match(new RegExp(`^      ${input}: \\$\\{\\{ (.+) \\}\\}$`, 'm'))?.[1];
      assert.ok(expression, input);
      const value = Function('needs', 'vars', `return ${expression}`);
      for (const agent of ['fake', 'claude', 'opencode', 'vibe']) {
        for (const config of ['', 'operator-config']) {
          const { needs = {}, vars = {} } = context(agent);
          assert.equal(value(needs, { ...vars, [variable]: config }),
            ['opencode', 'vibe'].includes(agent) ? config : '', `${input} ${agent}`);
        }
      }
    }
  }
});

// `agentic-job dispatch` decides the skills output (crates/agentic-job/src/dispatch.rs).
test('the skills output reaches agent-skills', () => {
  const expression = workflow.match(/^      agent-skills: \$\{\{ (.+) \}\}$/m)?.[1];
  assert.equal(expression, "needs.target.outputs.skills == 'true'");
});

test('the URL of what apply made is an output of dispatch, by way of the wrapper', () => {
  const read = file => fs.readFileSync(path.join(root, `.github/workflows/${file}`), 'utf8');
  for (const [file, value] of [['dispatch.yml', 'jobs.run.outputs.result-url'],
    ['agentic-job.yml', 'jobs.apply.outputs.result-url'], ['apply.yml', 'jobs.apply.outputs.result-url']]) {
    const outputs = (file === 'dispatch.yml' ? read(file).split('  workflow_call:\n')[1] : read(file)).split('\n    outputs:\n')[1];
    assert.match(outputs, new RegExp(`^      result-url:\n        description: .+\n        value: \\$\\{\\{ ${value.replaceAll('.', '\\.')} \\}\\}$`, 'm'), file);
  }
  assert.match(read('apply.yml'), /^      result-url: \$\{\{ steps\.result\.outputs\.result-url \}\}$/m);
});

test('shipped caller inputs exist in the same-commit reusable workflow', () => {
  const reusable = fs.readFileSync(path.join(root, '.github/workflows/agentic-job.yml'), 'utf8');
  const inputs = new Set([...reusable.split('    inputs:\n')[1].split(/^    outputs:\n/m)[0]
    .matchAll(/^      ([-\w]+):$/gm)].map(match => match[1]));
  const call = workflow.split('    with:\n').at(-1);
  for (const match of call.matchAll(/^      ([-\w]+):/gm)) assert.ok(inputs.has(match[1]), match[1]);
});

test('CI isolates proposed dispatch verifier code from write-capable cleanup', () => {
  const ci = fs.readFileSync(path.join(root, '.github/workflows/ci.yml'), 'utf8');
  const job = name => {
    const block = ci.match(new RegExp(`^  ${name}:\\n[\\s\\S]*?(?=^  [\\w-]+:|$(?![\\s\\S]))`, 'm'))?.[0];
    assert.ok(block, name);
    return block;
  };
  const verifier = job('e2e-dispatch-verify');
  const cleanup = job('e2e-verify');
  assert.equal(verifier.match(/    permissions:\n([\s\S]*?)(?=^    \S)/m)?.[1],
    '      contents: read\n      issues: read\n      pull-requests: read\n' +
    '      actions: read # read hosted job durations; no write or cleanup authority\n');
  assert.match(verifier, /ref: \$\{\{ github.sha \}\}/);
  assert.match(verifier, /persist-credentials: false/);
  // The static build of this revision chooses the packages the dispatch had.
  assert.match(verifier, /\n {8}with:\n {10}name: agentic-job\n/);
  assert.match(verifier, /"\$RUNNER_TEMP\/agentic-job" config --host --from "\$CONFIG" --repo "\$GH_REPO"\)/);
  // The verifier reads the configuration the scripted dispatch ran with.
  assert.match(verifier, /\n {10}CONFIG: \.github\/agentic-job\/hosted\.toml\n/);
  assert.match(fs.readFileSync(path.join(root, '.github/workflows/dispatch.yml'), 'utf8'),
    /\|\| '\.agentic-job-source\/\.github\/agentic-job\/hosted\.toml' \}\}/);
  assert.match(verifier, /GH_TOKEN: \$\{\{ github.token \}\}/);
  assert.match(verifier, /needs: \[changes, static, e2e-dispatch, e2e-compose, e2e-full\]/);
  assert.match(verifier, /if: \$\{\{ always\(\) && needs.changes.outputs.e2e == 'true' \}\}/);
  assert.match(cleanup, /needs: \[.*e2e-dispatch-verify\]/);
  assert.match(cleanup, /if: \$\{\{ always\(\) && needs.changes.outputs.e2e == 'true' \}\}/);
  assert.doesNotMatch(cleanup, /uses: actions\/checkout@|agentic-job config|github.sha|\.dispatch-verifier/);
  for (const name of ['Delete the comments', 'Close only the scratch pull request of this run and delete its branch']) {
    assert.ok(cleanup.includes(`- name: ${name}\n        if: \$\{\{ always() `) ||
      cleanup.includes(`- name: ${name}\n        if: \$\{\{ always() }}`), name);
  }
  const aggregate = job('required-checks');
  assert.match(aggregate, /needs: \[.*e2e-dispatch-verify.*\]/);
  // A fork's pull request is no reason to pass a skipped job: it fails ci.
  assert.doesNotMatch(aggregate, /\$fork\b/);
  assert.match(aggregate, /FILTER: >-\n +\$fork_e2e == "false" and all\(/);
});

test('CI runs do not share a dispatch or compose concurrency group, and a real dispatch keeps one per item', () => {
  // CI names a fixed item: without its run id, one queue entry's pending
  // call cancels another's and fails its e2e-dispatch-verify (#544).
  assert.match(workflow, /^  group: dispatch-\$\{\{ inputs\.kind \}\}-\$\{\{ inputs\.repo \}\}-\$\{\{ inputs\.item-repo \}\}-\$\{\{ inputs\.item \}\}\$\{\{ inputs\.scripted && format\('-\{0\}', github\.run_id\) \|\| '' \}\}\n  cancel-in-progress: false$/m);
  const compose = fs.readFileSync(path.join(root, '.github/workflows/example-compose.yml'), 'utf8');
  assert.match(compose, /^  group: compose-\$\{\{ inputs\.item \}\}\$\{\{ inputs\.scripted && format\('-\{0\}', github\.run_id\) \|\| '' \}\}\n  cancel-in-progress: false$/m);
  const ci = fs.readFileSync(path.join(root, '.github/workflows/ci.yml'), 'utf8');
  for (const [job, file] of [['e2e-dispatch', 'dispatch'], ['e2e-compose', 'example-compose']]) {
    const caller = ci.split(`\n  ${job}:\n`)[1].split(/\n  [a-z][a-z-]*:\n/)[0];
    assert.match(caller, new RegExp(`uses: \\./\\.github/workflows/${file}\\.yml[^\\n]*\\n    with:\\n(?:      [^\\n]*\\n)*?      scripted: true\\n`), job);
  }
  // A 5xx listing the run's jobs is retried rather than failing ci.
  const bound = ci.split('- name: Bound the hosted end-to-end agent job\n')[1].split('\n      - ')[0];
  assert.match(bound, /read_api\(\) \{ local out; for try in 1 2 3; do out=\$\(gh api "\$@"\) && \{ printf '%s\\n' "\$out"; return; \}; sleep 5; done; return 1; \}/);
  assert.equal(bound.match(/\bgh api\b/g).length, 1);
});

for (const profile of ['implement', 'comment', 'review']) {
  test(`shipped ${profile} setup produces a working session and hand-back`, () => {
    const home = fs.mkdtempSync(path.join(os.homedir(), 'dispatch-test-'));
    const cwd = path.join(home, 'work');
    // A rustup cargo finds its toolchains from the real home.
    const env = { ...process.env, HOME: home, RUSTUP_HOME: process.env.RUSTUP_HOME ?? path.join(os.homedir(), '.rustup') };
    try {
      fs.mkdirSync(cwd);
      fs.mkdirSync(path.join(home, 'out'));
      execFileSync('sh', [path.join(root, `workflow/dispatch-${profile}.sh`)], { env });
      execFileSync('git', ['init', '-q', cwd]);
      if (profile === 'review') {
        // A Rust target, which the review session runs cargo on: cargo must be on PATH.
        fs.mkdirSync(path.join(cwd, 'src'));
        fs.writeFileSync(path.join(cwd, 'src/lib.rs'), '');
        fs.writeFileSync(path.join(cwd, 'Cargo.toml'), '[package]\nname = "trial"\nversion = "0.1.0"\nedition = "2021"\n');
        fs.writeFileSync(path.join(cwd, 'Cargo.lock'), 'version = 4\n\n[[package]]\nname = "trial"\nversion = "0.1.0"\n');
        execFileSync('git', ['add', '.'], { cwd });
      }
      execFileSync('git', ['-c', 'user.name=Trial', '-c', 'user.email=trial@example.org',
        'commit', '--allow-empty', '-qm', 'Fixture'], { cwd });
      const steps = JSON.parse(fs.readFileSync(path.join(home, '.config/fake-agent/demo.json'), 'utf8'));
      for (const step of steps) {
        if (step.write) {
          const file = step.write.path.replaceAll('{home}', home).replaceAll('{cwd}', cwd);
          fs.writeFileSync(file, step.write.content);
        } else if (step.execute) {
          execFileSync('sh', ['-c', step.execute.command], { env, cwd });
        } else assert.fail('Unexpected shipped session step');
      }
      const outcome = JSON.parse(fs.readFileSync(path.join(home, 'out/outcome.json'), 'utf8'));
      assert.equal(outcome.stopped_early, null);
      assert.ok(outcome.summary);
      if (profile === 'implement') {
        assert.match(fs.readFileSync(path.join(cwd, 'DISPATCH-TRIAL.md'), 'utf8'), /No model/);
      } else {
        const output = JSON.parse(fs.readFileSync(path.join(home, 'out/safe-outputs.jsonl'), 'utf8'));
        assert.equal(output.type, 'add_comment');
        assert.deepEqual(Object.keys(output).sort(), ['body', 'type']);
        if (profile === 'review') {
          // The verdict line `agentic-job review verdict` reads in check.
          assert.match(output.body, /^VERDICT: (APPROVE|CHANGES)\n/);
          assert.ok(output.body.includes(execFileSync('git', ['rev-parse', 'HEAD'], { cwd, encoding: 'utf8' }).trim()));
          assert.match(output.body, /\nToolchain: cargo \d/);
          assert.equal(execFileSync('git', ['status', '--porcelain'], { cwd, encoding: 'utf8' }), '');
        }
      }
    } finally {
      fs.rmSync(home, { recursive: true, force: true });
    }
  });
}
