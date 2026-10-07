// The network rules and the egress proxy's files against the old tree's.
//
// The ruleset `sandbox setup` loads must be, text for text, what the old
// tree's setup-runner-sandbox.mjs generates for the same uids, direct
// endpoints and proxy uid. That script cannot be imported (it runs as it
// loads), so its constants and its two functions are cut out of its
// source and loaded as a module of their own. And egress/ here must be
// agent/egress/ there, byte for byte: the proxy was moved, not changed.
// Its pin is the exception: the old tree's requirements.txt, which names
// mitmproxy alone, is requirements.in here, and requirements.txt is
// generated from it with every dependency and the hashes of its files.
//
//   OLD_TREE=... AGENTIC_JOB=.../agentic-job node --test nft.test.mjs
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { test } from "node:test";
import { fileURLToPath, pathToFileURL } from "node:url";

const env = (name) => {
  assert.ok(process.env[name], `${name} must be set; see the top of this file`);
  return resolve(process.env[name]);
};
const OLD_TREE = env("OLD_TREE");
const AGENTIC_JOB = env("AGENTIC_JOB");
const EGRESS = join(dirname(fileURLToPath(import.meta.url)), "../../../../egress");
const PINS = "requirements.txt";
const PINS_SOURCE = "requirements.in";

// From the first constant to the end of sandboxRules.
const source = readFileSync(join(OLD_TREE, "scripts/setup-runner-sandbox.mjs"), "utf8");
const start = source.indexOf("const SANDBOX_GROUPS");
const end = source.indexOf("// The environment runner-sandbox's commands get");
assert.ok(start > 0 && end > start, "the old script is not laid out as this test expects");
const module = join(mkdtempSync(join(tmpdir(), "old-rules-")), "rules.mjs");
writeFileSync(module, `import { isIPv4 } from "node:net";\n${source.slice(start, end)}\nexport { sandboxRules, tailnetEndpoint };\n`);
const old = await import(pathToFileURL(module));

const UIDS = ["1002", "165536-231071"];
const BROKER = "http://100.101.102.103:18080/v1";
// uids, direct URLs, the proxy's uid
const CASES = [
  [["1002"], [], undefined],
  [UIDS, [], undefined],
  [UIDS, [BROKER], undefined],
  [UIDS, [], 993],
  [UIDS, [BROKER], 993],
  [UIDS, [BROKER, "https://100.64.0.1", "http://100.127.255.254:8080"], 993],
  [["1002", "165536-231071", "300000-300999"], [BROKER], 61234],
];

for (const [uids, direct, proxy] of CASES) {
  test(`rules for ${uids.length} uid ranges, ${direct.length} direct, proxy ${proxy ?? "none"}`, () => {
    const args = ["sandbox", "nft-rules", ...uids.flatMap((u) => ["--uid", u]), ...direct.flatMap((d) => ["--direct", d]),
      ...(proxy === undefined ? [] : ["--proxy-uid", String(proxy)])];
    const r = spawnSync(AGENTIC_JOB, args, { encoding: "utf8" });
    assert.equal(r.status, 0, r.stderr);
    assert.equal(r.stdout, old.sandboxRules(uids, direct.map(old.tailnetEndpoint), proxy));
  });
}

test("both refuse what is not a tailnet endpoint", () => {
  for (const url of ["http://10.0.0.1:80", "http://proxy.example:18080", "ftp://100.101.102.103", "http://100.128.0.1"]) {
    assert.throws(() => old.tailnetEndpoint(url), undefined, url);
    const r = spawnSync(AGENTIC_JOB, ["sandbox", "nft-rules", "--uid", "1002", "--direct", url], { encoding: "utf8" });
    assert.equal(r.status, 2, `${url}: ${r.stdout}`);
  }
});

test("egress/ is the old tree's agent/egress/, unchanged", () => {
  const theirs = join(OLD_TREE, "agent/egress");
  const names = readdirSync(theirs).sort();
  assert.deepEqual(readdirSync(EGRESS).filter((name) => name !== PINS_SOURCE).sort(), names);
  for (const name of names.filter((name) => name !== PINS)) {
    assert.ok(readFileSync(join(EGRESS, name)).equals(readFileSync(join(theirs, name))), `${name} differs`);
  }
  const pinned = (file) => readFileSync(file, "utf8").split("\n").filter((line) => line && !line.startsWith("#"));
  assert.deepEqual(pinned(join(EGRESS, PINS_SOURCE)), pinned(join(theirs, PINS)));
});
