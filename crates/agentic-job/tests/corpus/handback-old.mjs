// Runs the old tree's hand-back on a checkout, for tests/handback.rs:
//   node handback-old.mjs OLD_TREE REQUEST
// REQUEST is a JSON file with the arguments of its writeHandback but `git`,
// which is made here as agent/run.mjs makes it, and `workdir`, the checkout.
// Prints what it returns: what summary.json says of the patch.
import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { join } from "node:path";

const [oldTree, requestFile] = process.argv.slice(2);
const { writeHandback } = await import(join(oldTree, "agent/handback.mjs"));
const request = JSON.parse(readFileSync(requestFile, "utf8"));
const settings = ["-c", "core.fsmonitor=false", "-c", "core.hooksPath=/dev/null"];
const git = (args, { limit } = {}) => {
  const r = spawnSync("git", [...settings, "-C", request.workdir, ...args], { maxBuffer: 1 << 28 });
  return { status: r.status, stdout: limit ? r.stdout.subarray(0, limit) : r.stdout };
};
console.log(JSON.stringify(writeHandback({ ...request, git })));
