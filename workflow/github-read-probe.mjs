// Credential-free contract probe. Run only against a gateway configured with
// a loopback mock upstream, never against GitHub or a credential-bearing proxy.
import assert from 'node:assert/strict';
import { pathToFileURL } from 'node:url';

export const cases = [
  { name: 'REST read', method: 'GET', path: '/repos/owner/repo/issues/1', allowed: true },
  { name: 'GraphQL query', method: 'POST', path: '/graphql', allowed: true,
    body: { query: 'query { repository(owner:"owner", name:"repo") { issue(number:1) { title } } }' } },
  { name: 'REST comment write', method: 'POST', path: '/repos/owner/repo/issues/1/comments',
    body: { body: 'contract probe' } },
  { name: 'REST update', method: 'PATCH', path: '/repos/owner/repo/issues/1', body: { title: 'probe' } },
  { name: 'REST delete', method: 'DELETE', path: '/repos/owner/repo/issues/comments/1' },
  { name: 'REST put', method: 'PUT', path: '/repos/owner/repo/issues/1/lock', body: {} },
  { name: 'GraphQL mutation', method: 'POST', path: '/graphql',
    body: { query: 'mutation { addComment(input:{subjectId:"mock",body:"probe"}) { clientMutationId } }' } },
  { name: 'out-of-bounds repository', method: 'GET', path: '/repos/other/private/issues/1' },
  { name: 'out-of-bounds GraphQL repository', method: 'POST', path: '/graphql',
    body: { query: 'query { repository(owner:"other", name:"private") { issue(number:1) { title } } }' } },
  { name: 'unmapped repository route', method: 'GET', path: '/repos/other/private/unknown-route' },
];

function loopback(value) {
  const url = new URL(value);
  assert.equal(url.protocol, 'http:', 'probe requires a plain HTTP mock setup');
  assert.equal(url.hostname, '127.0.0.1', 'probe requires IPv4 loopback');
  assert.equal(url.username + url.password + url.search + url.hash, '', 'unexpected URL components');
  assert.equal(url.pathname, '/', 'probe requires an origin URL');
  return url.origin;
}

// The independent mock observer exposes GET /observed as an array of
// {method, path}. It records requests before responding, including failures.
// A 403 alone is insufficient: GitHub may have refused a forwarded write.
export async function probe(gateway, observer) {
  gateway = loopback(gateway);
  observer = loopback(observer);
  assert.notEqual(gateway, observer, 'gateway and observer must be separate');
  const observed = async () => {
    const response = await fetch(`${observer}/observed`, { signal: AbortSignal.timeout(5000), redirect: 'error' });
    assert.equal(response.status, 200, 'mock observation failed');
    const records = await response.json();
    assert.ok(Array.isArray(records), 'mock observations must be an array');
    return records;
  };
  for (const item of cases) {
    const before = await observed();
    const response = await fetch(`${gateway}${item.path}`, {
      method: item.method, redirect: 'error', signal: AbortSignal.timeout(5000),
      headers: item.body ? { 'Content-Type': 'application/json' } : {},
      body: item.body ? JSON.stringify(item.body) : undefined,
    });
    await response.arrayBuffer();
    const after = await observed();
    if (item.allowed) {
      assert.equal(response.status, 200, `${item.name}: read failed`);
      assert.ok(after.slice(before.length).some(record =>
        record.method === item.method && record.path === item.path),
      `${item.name}: read did not reach mock upstream`);
    } else {
      assert.equal(response.status, 403, `${item.name}: gateway did not deny request`);
      assert.deepEqual(after, before, `${item.name}: denied request reached upstream`);
    }
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  try {
    assert.equal(process.argv.length, 4, 'usage: node workflow/github-read-probe.mjs GATEWAY_ORIGIN MOCK_OBSERVER_ORIGIN');
    await probe(process.argv[2], process.argv[3]);
    console.log('GitHub read gateway contract passed (mock upstream only)');
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
