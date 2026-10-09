import assert from 'node:assert/strict';
import http from 'node:http';
import { once } from 'node:events';
import { test } from 'node:test';
import { cases, probe } from './github-read-probe.mjs';

async function listen(t, handler) {
  const server = http.createServer(handler);
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  t.after(() => { server.closeAllConnections(); server.close(); });
  return `http://127.0.0.1:${server.address().port}`;
}

for (const behavior of ['safe', 'forward-write', 'upstream-denial', 'mutation-denial', 'graphql-bounds', 'unmapped-read', 'fake-read']) {
  test(`contract probe: ${behavior}`, async t => {
    const records = [];
    const observer = await listen(t, (_request, response) => {
      response.setHeader('Content-Type', 'application/json');
      response.end(JSON.stringify(records));
    });
    const gateway = await listen(t, async (request, response) => {
      let body = '';
      for await (const chunk of request) body += chunk;
      const item = cases.find(item => item.method === request.method && item.path === request.url
        && (item.body ? JSON.stringify(item.body) : '') === body);
      const forwardWrite = behavior === 'forward-write' || behavior === 'upstream-denial';
      const forwarded = item.allowed && behavior !== 'fake-read'
        || !item.allowed && (forwardWrite
          || behavior === 'mutation-denial' && item.name === 'GraphQL mutation'
          || behavior === 'graphql-bounds' && item.name === 'out-of-bounds GraphQL repository'
          || behavior === 'unmapped-read' && item.name === 'unmapped repository route');
      if (forwarded) records.push({ method: request.method, path: request.url });
      response.statusCode = item.allowed || forwarded && !['upstream-denial', 'mutation-denial'].includes(behavior) ? 200 : 403;
      response.end('{}');
    });
    if (behavior === 'safe') await probe(gateway, observer);
    else {
      const expected = {
        'forward-write': /REST comment write: gateway did not deny request/,
        'upstream-denial': /REST comment write: denied request reached upstream/,
        'mutation-denial': /GraphQL mutation: denied request reached upstream/,
        'graphql-bounds': /out-of-bounds GraphQL repository: gateway did not deny request/,
        'unmapped-read': /unmapped repository route: gateway did not deny request/,
        'fake-read': /REST read: read did not reach mock upstream/,
      };
      await assert.rejects(probe(gateway, observer), expected[behavior]);
    }
  });
}

test('probe refuses non-loopback and credential-bearing URLs before connecting', async () => {
  for (const url of ['https://127.0.0.1:1', 'http://example.com', 'http://user:password@127.0.0.1', 'http://127.0.0.1/api']) {
    await assert.rejects(probe(url, 'http://127.0.0.1:2'));
  }
});
