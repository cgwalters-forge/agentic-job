'use strict';

// A run that pushes to a pull request starts from that pull request's
// branch at the head read here, with the policy job's read-only token:
// the number is the caller's input, the branch and the head are the
// forge's answer, and nothing of either comes from the agent or the
// pull request's text. `agentic-job policy` checks the branch against
// the caller's bounds and pins the head; check and apply hold the run to
// both.
async function resolvePush(env, github) {
  if (env.EVENT !== 'false' || env.REVIEW !== 'false' || env.KIND !== 'branch' ||
      // Apply applies outputs to the calling repository when none is named.
      (env.OUTPUT_REPO || env.GITHUB_REPOSITORY || '').toLowerCase() !== (env.REPO ?? '').toLowerCase() ||
      !/^[1-9][0-9]{0,8}$/.test(env.ITEM ?? '') ||
      !/^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/.test(env.REPO ?? '')) {
    throw new Error('A push to a pull request needs a branch run of no event or review, on a pull request number, with its outputs in the repository it works on');
  }
  const [owner, repo] = env.REPO.split('/');
  const { data } = await github.rest.pulls.get({ owner, repo, pull_number: Number(env.ITEM) });
  // A fork's branch is not this repository's to push to; gh-aw's handler
  // refuses one too.
  if (data.number !== Number(env.ITEM) || data.state !== 'open' ||
      data.base?.repo?.full_name !== env.REPO || data.head?.repo?.full_name !== env.REPO ||
      !/^[0-9a-f]{40}$/.test(data.head?.sha ?? '') || typeof data.head?.ref !== 'string') {
    throw new Error('A push needs an open pull request whose branch is in the repository it works on');
  }
  return { branch: data.head.ref, head: data.head.sha };
}

module.exports = { resolvePush };
