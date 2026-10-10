'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const { resolvePush } = require('./push.cjs');

test('a push resolves an open same-repository branch and refuses hostile routing', async () => {
  const env = { EVENT: 'false', REVIEW: 'false', KIND: 'branch', ITEM: '42', REPO: 'o/r', OUTPUT_REPO: '',
    GITHUB_REPOSITORY: 'o/r' };
  const data = { number: 42, state: 'open', base: { ref: 'main', repo: { full_name: 'o/r' } },
    head: { ref: 'agent-run-12', sha: 'a'.repeat(40), repo: { full_name: 'o/r' } } };
  const api = value => ({ rest: { pulls: { get: async args => {
    assert.deepEqual(args, { owner: 'o', repo: 'r', pull_number: 42 });
    return { data: value };
  } } } });
  assert.deepEqual(await resolvePush(env, api(data)), { branch: 'agent-run-12', head: 'a'.repeat(40) });
  for (const change of [{ OUTPUT_REPO: 'o/r' }, { OUTPUT_REPO: 'O/R' }, { OUTPUT_REPO: 'o/r', GITHUB_REPOSITORY: 'caller/repo' }]) {
    assert.deepEqual(await resolvePush({ ...env, ...change }, api(data)), { branch: 'agent-run-12', head: 'a'.repeat(40) });
  }
  // (the request's routing)
  for (const change of [{ EVENT: 'true' }, { REVIEW: 'true' }, { KIND: 'analysis' },
    { OUTPUT_REPO: 'other/repo' }, { GITHUB_REPOSITORY: 'caller/repo' }, { GITHUB_REPOSITORY: undefined },
    { ITEM: '01' }, { ITEM: '42 ' }, { ITEM: '' },
    { ITEM: '9999999999' }, { REPO: 'o/r/x' }]) {
    await assert.rejects(resolvePush({ ...env, ...change }, api(data)), /needs a branch run/, JSON.stringify(change));
  }
  // (the forge's answer)
  for (const change of [{ state: 'closed' }, { number: 43 },
    { base: { ref: 'main', repo: { full_name: 'other/r' } } },
    { head: { ...data.head, repo: { full_name: 'fork/r' } } },
    { head: { ...data.head, repo: null } },
    { head: { ...data.head, sha: 'A'.repeat(40) } },
    { head: { ...data.head, ref: undefined } }]) {
    await assert.rejects(resolvePush(env, api({ ...data, ...change })), /open pull request/, JSON.stringify(change));
  }
});
