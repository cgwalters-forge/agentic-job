'use strict';

const fs = require('node:fs');
const { execFileSync } = require('node:child_process');

function requiresE2e(event, eventName, git = args => execFileSync('git', args)) {
  if (!['pull_request', 'push'].includes(eventName)) return true;
  const base = eventName === 'pull_request' ? event.pull_request?.base?.sha : event.before;
  const head = eventName === 'pull_request' ? event.pull_request?.head?.sha : event.after;
  // Missing history or a new branch must run the suite, not guess at its paths.
  if (![base, head].every(sha => typeof sha === 'string' && /^[0-9a-f]{40}$/.test(sha) && !/^0+$/.test(sha))) return true;
  try {
    const range = eventName === 'pull_request' ? `${base}...${head}` : `${base}..${head}`;
    // No rename detection: both the old and new names must be classified.
    const names = git(['diff', '--no-ext-diff', '--no-renames', '--name-only', '-z', range, '--']);
    const paths = names.toString('utf8').split('\0');
    if (paths.pop() !== '') return true;
    return paths.length === 0 || paths.some(name =>
      name !== 'README.md' && !/^docs\/(?:[^/\0\r\n]+\/)*[^/\0\r\n]+\.md$/.test(name));
  } catch {
    return true;
  }
}

if (require.main === module) {
  const event = JSON.parse(fs.readFileSync(process.env.GITHUB_EVENT_PATH, 'utf8'));
  fs.appendFileSync(process.env.GITHUB_OUTPUT,
    `e2e=${requiresE2e(event, process.env.GITHUB_EVENT_NAME)}\n`);
}

module.exports = { requiresE2e };
