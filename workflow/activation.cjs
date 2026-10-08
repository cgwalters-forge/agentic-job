'use strict';

// Discussions have their own namespace; no issue handler may see their number.
function admit(decision) {
  if (decision.item?.kind === 'discussion') {
    return { ...decision, admitted: false,
      reason: 'discussion notification and output routing are not supported' };
  }
  return decision;
}

// The policy artifact comes from a trusted job, never from the agent.
async function activate(decision, github, repository) {
  if (!decision.admitted) return;
  if (decision.item?.kind === 'discussion') {
    throw new Error('Discussion notification and output routing are not supported');
  }
  if (decision.action !== 'labeled' || !decision.command) return;
  if (!['issue', 'pull_request'].includes(decision.item?.kind) ||
      !Number.isSafeInteger(decision.item.number) || decision.item.number <= 0) {
    throw new Error('Label activation requires an issue or pull request identity');
  }
  await github.rest.issues.removeLabel({
    ...repository,
    issue_number: decision.item.number,
    name: decision.command,
  });
}

module.exports = { admit, activate };
