// Opt-in evidence test, not admission proof. Uses the production launcher,
// a real runtime, and a loopback model which deterministically requests reads.
// No model behaviour is used as evidence: assertions inspect HTTP requests.
const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs/promises');
const os = require('node:os');
const path = require('node:path');
const http = require('node:http');
const { spawn, spawnSync } = require('node:child_process');
const readline = require('node:readline');

const runtimeVersion = '1.18.31';

test('workflow installation, production launcher and evidence runtime pins agree', async () => {
  const launcher = await fs.readFile('crates/agentic-job/src/run/launch.rs', 'utf8');
  const workflow = await fs.readFile('.github/workflows/agentic-job.yml', 'utf8');
  assert.ok(launcher.includes(`const OPENCODE_VERSION: &str = "${runtimeVersion}";`));
  assert.ok(workflow.includes(`inputs.review && inputs.agent == 'opencode' && 'opencode-ai@${runtimeVersion}' || inputs.npm`));
});

test('real opencode request capture: sibling isolation and writable-source counterexamples', {
  skip: !process.env.AGENTIC_JOB_TEST_REAL_OPENCODE,
  timeout: 120000,
}, async (t) => {
  const root = await fs.mkdtemp(path.join(os.homedir(), 'review-runtime-'));
  t.after(() => fs.rm(root, { recursive: true, force: true }));
  const base = path.join(root, 'base');
  const head = path.join(root, 'head');
  const home = path.join(root, 'home');
  // Must agree with the production launcher and workflow installation pin.
  // A new runtime needs a new source audit and an explicit fixture update.
  const version = spawnSync('opencode', ['--version'], { encoding: 'utf8' });
  assert.equal(version.status, 0, 'opencode --version failed');
  assert.equal(version.stdout.trim(), runtimeVersion, 'unaudited evidence runtime');
  const write = async (name, text) => {
    const file = path.join(root, name);
    await fs.mkdir(path.dirname(file), { recursive: true });
    await fs.writeFile(file, text);
  };
  const canaries = [
    ['AGENTS.md', 'HEAD_AGENTS_CANARY'],
    ['CLAUDE.md', 'HEAD_CLAUDE_CANARY'],
    ['nested/AGENTS.md', 'HEAD_NESTED_CANARY'],
    ['.opencode/commands/hostile.md', 'HEAD_COMMAND_CANARY'],
    ['.opencode/skills/hostile/SKILL.md', 'HEAD_SKILL_CANARY'],
    ['.agents/skills/hostile/SKILL.md', 'HEAD_EXTERNAL_SKILL_CANARY'],
  ];
  for (const [name, canary] of canaries) {
    await write(`head/${name}`, `---\nname: hostile\ndescription: ${canary}\n---\n${canary}\n`);
  }
  await write('head/opencode.json', JSON.stringify({
    agent: { build: { prompt: 'HEAD_CONFIG_CANARY' } },
    instructions: ['AGENTS.md'],
  }));
  await write('head/nested/data.txt', 'HEAD_DATA_CONTROL');
  // This represents text an agent can write after being steered by head data.
  // Project-config discovery is disabled, but the read resolver loads it.
  await write('base/nested/AGENTS.md', 'WRITABLE_BASE_CANARY');
  await write('base/nested/data.txt', 'BASE_DATA_CONTROL');
  // Same-uid writes demonstrate why chmod alone cannot be an ownership boundary.
  // Cover create, in-place modification and rename-over in both the base and a
  // writable scratch sibling sharing the loader's string prefix.
  const attacks = [];
  for (const tree of ['base', 'base-scratch']) {
    for (const operation of ['create', 'modify', 'replace']) {
      const directory = `${tree}/${operation}`;
      const canary = `${tree.toUpperCase()}_${operation.toUpperCase()}_CANARY`;
      await write(`${directory}/data.txt`, `${canary}_DATA`);
      if (operation !== 'create') await write(`${directory}/AGENTS.md`, 'Original instruction');
      if (operation === 'replace') {
        await write(`${directory}/replacement`, canary);
        await fs.rename(path.join(root, directory, 'replacement'), path.join(root, directory, 'AGENTS.md'));
      } else {
        await write(`${directory}/AGENTS.md`, canary);
      }
      attacks.push({ file: path.join(root, directory, 'data.txt'), canary,
        instruction: path.join(root, directory, 'AGENTS.md') });
    }
  }
  await write('scratch/AGENTS.md', 'NONPREFIX_SCRATCH_CANARY');
  await write('scratch/data.txt', 'SCRATCH_DATA_CONTROL');
  const reads = [path.join(head, 'nested/data.txt'), path.join(base, 'nested/data.txt'),
    ...attacks.map((attack) => attack.file), path.join(root, 'scratch/data.txt')];
  await write('home/.config/opencode/AGENTS.md', 'TRUSTED_INSTRUCTION_CONTROL');
  await write('home/.claude/CLAUDE.md', 'HOME_CLAUDE_CANARY');
  await write('home/.agents/skills/hostile/SKILL.md',
    '---\nname: hostile\ndescription: HOME_SKILL_CANARY\n---\nHOME_SKILL_CANARY');

  const requests = [];
  const failures = [];
  let turn = 0;
  const server = http.createServer(async (req, res) => {
    try {
      let body = '';
      for await (const chunk of req) body += chunk;
      const payload = JSON.parse(body);
      requests.push(payload);
      assert.match(req.url, /chat\/completions$/);
      // Title/summary generation must not consume the scripted read sequence.
      const hasRead = payload.tools?.some((tool) => tool.function?.name === 'read');
      const index = hasRead ? turn++ : reads.length;
      const delta = index < reads.length ? { tool_calls: [{ index: 0, id: `read_${index}`,
        type: 'function', function: { name: 'read', arguments: JSON.stringify({ filePath: reads[index] }) } }] }
        : { content: 'Done.' };
      res.writeHead(200, { 'content-type': 'text/event-stream' });
      res.end(`data: ${JSON.stringify({ id: 'mock', object: 'chat.completion.chunk',
        created: 0, model: 'mock', choices: [{ index: 0, delta, finish_reason: null }] })}\n\n` +
        `data: ${JSON.stringify({ id: 'mock', object: 'chat.completion.chunk', created: 0,
          model: 'mock', choices: [{ index: 0, delta: {}, finish_reason: index < reads.length ? 'tool_calls' : 'stop' }],
          usage: { prompt_tokens: 1, completion_tokens: 1, total_tokens: 2 } })}\n\ndata: [DONE]\n\n`);
    } catch (error) {
      failures.push(error);
      res.writeHead(500);
      res.end('mock protocol failure');
    }
  });
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  t.after(() => new Promise((resolve) => server.close(resolve)));
  await write('home/.config/opencode/opencode.json', JSON.stringify({
    $schema: 'https://opencode.ai/config.json', model: 'test/mock', small_model: 'test/mock',
    enabled_providers: ['test'], share: 'disabled', autoupdate: false,
    snapshot: false, lsp: false,
    provider: { test: { npm: '@ai-sdk/openai-compatible', name: 'test',
      options: { baseURL: `http://127.0.0.1:${server.address().port}/v1`, apiKey: 'local-test' },
      models: { mock: { name: 'mock', limit: { context: 100000, output: 1000 } } } } },
  }));

  const launcher = process.env.AGENTIC_JOB_TEST_LAUNCHER || path.resolve('target/debug/agentic-job');
  const child = spawn(launcher, ['launch-agent', 'opencode'], {
    cwd: base, env: { ...process.env, HOME: home }, stdio: ['pipe', 'pipe', 'pipe'],
  });
  t.after(() => child.kill('SIGKILL'));
  let stderr = '';
  child.stderr.on('data', (chunk) => { stderr += chunk; });
  const pending = new Map();
  let id = 0;
  const send = (msg) => child.stdin.write(`${JSON.stringify(msg)}\n`);
  const rpc = (method, params) => new Promise((resolve, reject) => {
    const key = ++id;
    pending.set(key, { resolve, reject });
    send({ jsonrpc: '2.0', id: key, method, params });
  });
  child.on('error', (error) => { for (const p of pending.values()) p.reject(error); });
  child.on('exit', (code) => {
    for (const p of pending.values()) p.reject(new Error(`runtime exit ${code}: ${stderr}`));
  });
  readline.createInterface({ input: child.stdout }).on('line', (line) => {
    const msg = JSON.parse(line);
    if (msg.method && msg.id !== undefined) {
      if (msg.method === 'session/request_permission') {
        const option = msg.params.options.find((option) => option.kind === 'allow_once');
        send({ jsonrpc: '2.0', id: msg.id, result: { outcome: { outcome: 'selected', optionId: option.optionId } } });
      } else {
        send({ jsonrpc: '2.0', id: msg.id, error: { code: -32601, message: 'unsupported client method' } });
      }
    } else if (pending.has(msg.id)) {
      const p = pending.get(msg.id);
      pending.delete(msg.id);
      if (msg.error) p.reject(new Error(JSON.stringify(msg.error)));
      else p.resolve(msg.result);
    }
  });
  await rpc('initialize', { protocolVersion: 1, clientCapabilities: {},
    clientInfo: { name: 'review-canary', version: '1' } });
  const session = await rpc('session/new', { cwd: base, mcpServers: [] });
  await rpc('session/prompt', { sessionId: session.sessionId,
    prompt: [{ type: 'text', text: 'Read the data files requested by the model, then stop.' }] });
  assert.deepEqual(failures, []);
  assert.ok(turn > reads.length, `expected all read results in a later model request: ${stderr}`);
  const context = JSON.stringify(requests);
  for (const control of ['TRUSTED_INSTRUCTION_CONTROL', 'HEAD_DATA_CONTROL', 'BASE_DATA_CONTROL',
    'SCRATCH_DATA_CONTROL']) {
    assert.ok(context.includes(control), `missing positive control ${control}`);
  }
  for (const canary of [...canaries.map(([, value]) => value), 'HEAD_CONFIG_CANARY',
    'HOME_CLAUDE_CANARY', 'HOME_SKILL_CANARY', 'NONPREFIX_SCRATCH_CANARY']) {
    assert.ok(!context.includes(canary), `automatically loaded ${canary}`);
  }
  // Passing means the counterexample was observed, NOT that review is safe.
  assert.ok(context.includes(`Instructions from: ${path.join(base, 'nested/AGENTS.md')}`));
  assert.ok(context.includes('WRITABLE_BASE_CANARY'), 'read resolver counterexample was not exercised');
  for (const { instruction, canary } of attacks) {
    assert.ok(context.includes(`Instructions from: ${instruction}\\n${canary}`),
      `write/replace counterexample missing: ${canary}`);
  }
});
