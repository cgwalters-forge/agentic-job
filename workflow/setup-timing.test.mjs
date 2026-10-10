import assert from 'node:assert/strict';
import { test } from 'node:test';
import { checkSetupStages } from './setup-timing.mjs';

const stage = name => `Sandbox setup stage ${name}: 0.123s (ok)\n`;
const allowlist = stage('privileged-binary-allowlist');
const view = stage('filesystem-view') + allowlist;
const walk = stage('world-write-and-setuid-walk') + stage('setuid-package-ownership') + allowlist;

for (const [name, log, mode, valid] of [
  ['view', view, 'false', true],
  ['walk', walk, 'true', true],
  ['view accidentally traverses', view + walk, 'false', false],
  ['walk missing allowlist', walk.replace(allowlist, ''), 'true', false],
  ['walk missing setuid ownership', walk.replace(stage('setuid-package-ownership'), ''), 'true', false],
  ['view missing allowlist', stage('filesystem-view'), 'false', false],
  ['view has old setuid search', view + stage('setuid-package-ownership'), 'false', false],
  ['missing view', '', 'false', false],
  ['failed stage', view.replace('(ok)', '(failed)'), 'false', false],
  ['duplicate stage', view + stage('filesystem-view'), 'false', false],
  ['invalid mode', view, 'invalid', false],
]) {
  test(name, () => {
    if (valid) {
      assert.doesNotThrow(() => checkSetupStages(log, mode));
    } else {
      assert.throws(() => checkSetupStages(log, mode));
    }
  });
}
