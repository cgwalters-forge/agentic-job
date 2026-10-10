import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { pathToFileURL } from 'node:url';

export function checkSetupStages(log, legacyWalk) {
  assert.ok(['true', 'false'].includes(legacyWalk), 'invalid legacy-walk value');
  const walk = legacyWalk === 'true';
  for (const [stage, expected] of [
    ['privileged-binary-allowlist', true],
    ['world-write-and-setuid-walk', walk],
    ['setuid-package-ownership', walk],
    ['filesystem-view', !walk],
  ]) {
    const lines = log.split('\n').filter(line => line.startsWith(`Sandbox setup stage ${stage}:`));
    assert.equal(lines.length, expected ? 1 : 0, `unexpected setup stage: ${stage}`);
    if (expected) {
      assert.match(lines[0], /: \d+\.\d{3}s \(ok\)$/, `unsuccessful setup stage: ${stage}`);
    }
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  assert.equal(process.argv.length, 4, 'usage: setup-timing.mjs LOG LEGACY_WALK');
  checkSetupStages(readFileSync(process.argv[2], 'utf8'), process.argv[3]);
}
