'use strict';

// Called by the Rust integration test with an actual event CLI artifact.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const { admit, activate } = require('./activation.cjs');

async function main() {
  const decision = admit(JSON.parse(fs.readFileSync(process.argv[2], 'utf8')));
  const expectedWrites = Number(process.argv[3]);
  let writes = 0;
  const github = { rest: { issues: { removeLabel: async (request) => {
    writes++;
    assert.deepEqual(request, {
      owner: 'cgwalters-forge', repo: 'agentic-job', issue_number: 15, name: 'agent-review',
    });
  } } } };
  await activate(decision, github, { owner: 'cgwalters-forge', repo: 'agentic-job' });
  assert.equal(writes, expectedWrites);
}

main().catch(error => { console.error(error); process.exitCode = 1; });
