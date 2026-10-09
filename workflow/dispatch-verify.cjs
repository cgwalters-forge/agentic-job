'use strict';

// Proposed CI code runs with read-only permissions; cleanup is a separate job.
const assert = require('node:assert/strict');
const { execFileSync } = require('node:child_process');

const { GH_REPO: repo, RUN: run, PR: pr, REVIEW_HEAD: head, GITHUB_RUN_ID: id, RESULT: result } = process.env;
assert.equal(result, 'success', 'all shipped dispatch profiles must succeed');
const api = endpoint => JSON.parse(execFileSync('gh', ['api', '--paginate', '--slurp', endpoint], { encoding: 'utf8' })).flat();
const comments = item => api(`repos/${repo}/issues/${item}/comments`)
  .filter(comment => comment.user.login === 'github-actions[bot]' && comment.body.includes(run));
assert.equal(comments(64).filter(comment => comment.body.includes('Scripted dispatch completed.')).length, 2);
if (pr) {
  assert.match(head, /^[0-9a-f]{40}$/);
  const verdicts = comments(pr).filter(comment => comment.body.startsWith('VERDICT: APPROVE\nREASON: Scripted dispatch'));
  assert.equal(verdicts.length, 1);
  assert.ok(verdicts[0].body.includes(`Reviewed SHA: ${head}`));
}
const branch = `dispatch/implement/agent-run-${id}`;
const commit = api(`repos/${repo}/commits/${branch}`)[0];
assert.deepEqual(commit.files.map(file => file.filename), ['DISPATCH-TRIAL.md']);
assert.equal(commit.parents.length, 1);
const pulls = api(`repos/${repo}/pulls?head=${encodeURIComponent(repo.split('/')[0] + ':' + branch)}&state=all`);
for (const pull of pulls) assert.equal(pull.draft, true);
