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

test('real opencode request capture: head instructions are not loaded at startup', {
  skip: !process.env.AGENTIC_JOB_TEST_REAL_OPENCODE,
  timeout: 120000,
}, async (t) => {
  const root = await fs.mkdtemp(path.join(os.homedir(), 'review-runtime-'));
  t.after(() => fs.rm(root, { recursive: true, force: true }));
  const base = path.join(root, 'base');
  const head = path.join(root, 'head');
  const home = path.join(root, 'home');
  const workflow = await fs.readFile('.github/workflows/agentic-job.yml', 'utf8');
  const runtimeVersion = workflow.match(/opencode-ai@([0-9.]+)/)[1];
  const version = spawnSync('opencode', ['--version'], { encoding: 'utf8' });
  assert.equal(version.status, 0, 'opencode --version failed');
  assert.equal(version.stdout.trim(), runtimeVersion, 'install the workflow runtime for this regression');
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
  await write('base/nested/data.txt', 'BASE_DATA_CONTROL');
  const reads = [path.join(head, 'nested/data.txt'), path.join(base, 'nested/data.txt')];
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
  for (const control of ['TRUSTED_INSTRUCTION_CONTROL', 'HEAD_DATA_CONTROL', 'BASE_DATA_CONTROL']) {
    assert.ok(context.includes(control), `missing positive control ${control}`);
  }
  for (const canary of [...canaries.map(([, value]) => value), 'HEAD_CONFIG_CANARY',
    'HOME_CLAUDE_CANARY', 'HOME_SKILL_CANARY']) {
    assert.ok(!context.includes(canary), `automatically loaded ${canary}`);
  }
});
