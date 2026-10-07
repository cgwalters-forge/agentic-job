// `node --test secure-host/binary.test.mjs`: the digest names content, modes and names
// and nothing else, and a file that is not the pinned one is refused.

import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { chmodSync, mkdirSync, mkdtempSync, renameSync, symlinkSync, utimesSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";

import { BUILT_FROM, NAMES, checked, digest, readPin } from "./binary.mjs";

function tree() {
  const root = mkdtempSync(join(tmpdir(), "source-"));
  for (const name of BUILT_FROM) {
    if (name.includes(".")) writeFileSync(join(root, name), name);
    else {
      mkdirSync(join(root, name, "src"), { recursive: true });
      writeFileSync(join(root, name, "src", "lib.rs"), "// lib\n");
    }
  }
  return root;
}

test("the digest is of content, modes and names, and of nothing else", () => {
  const changes = [
    ["a file's content", (root) => writeFileSync(join(root, "crates/src/lib.rs"), "// other\n"), true],
    ["a file's name", (root) => renameSync(join(root, "crates/src/lib.rs"), join(root, "crates/src/main.rs")), true],
    ["a new file", (root) => writeFileSync(join(root, "egress/new.py"), ""), true],
    ["the executable bit", (root) => chmodSync(join(root, "crates/src/lib.rs"), 0o755), true],
    ["a link", (root) => symlinkSync("lib.rs", join(root, "crates/src/link.rs")), true],
    ["Cargo.lock", (root) => writeFileSync(join(root, "Cargo.lock"), "changed"), true],
    ["a file's time", (root) => utimesSync(join(root, "crates/src/lib.rs"), 1, 1), false],
    ["group and other bits", (root) => chmodSync(join(root, "crates/src/lib.rs"), 0o600), false],
    ["an empty directory", (root) => mkdirSync(join(root, "crates/empty")), false],
    ["a file outside the source", (root) => writeFileSync(join(root, "README.md"), "x"), false],
  ];
  for (const [what, change, changes_it] of changes) {
    const root = tree();
    const before = digest(root);
    assert.equal(digest(tree()), before, "two copies of one source differ");
    change(root);
    assert.equal(digest(root) !== before, changes_it, what);
  }
});

test("a link is its target, not what the target holds", () => {
  const [one, other] = [tree(), tree()];
  symlinkSync("lib.rs", join(one, "crates/src/link.rs"));
  symlinkSync("../src/lib.rs", join(other, "crates/src/link.rs"));
  assert.notEqual(digest(one), digest(other));
});

test("a source with a part missing has no digest", () => {
  const root = tree();
  renameSync(join(root, "egress"), join(root, "elsewhere"));
  assert.throws(() => digest(root), /ENOENT/);
});

test("only the pinned bytes are taken", () => {
  const bytes = Buffer.from("the binary");
  const sum = createHash("sha256").update(bytes).digest("hex");
  const pin = { release: "v1", sha256: Object.fromEntries(NAMES.map((name) => [name, sum])) };
  assert.equal(checked(pin, NAMES[0], bytes), bytes);
  assert.throws(() => checked(pin, NAMES[0], Buffer.from("another binary")), /refusing it/);
});

test("the pin in this repository reads, and a release without checksums does not", () => {
  const pin = readPin();
  assert.match(pin.repository, /^[\w.-]+\/[\w.-]+$/);
  const path = join(mkdtempSync(join(tmpdir(), "pin-")), "release.json");
  writeFileSync(path, JSON.stringify({ ...pin, release: "v1", sha256: { [NAMES[0]]: "0".repeat(64) } }));
  assert.throws(() => readPin(path), /no checksum for/);
});
