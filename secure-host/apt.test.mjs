import assert from "node:assert/strict";
import { existsSync, mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { spawn } from "node:child_process";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { configureApt, NETWORK_CONFIG } from "./apt.mjs";

test("apt setup preserves fallback and source trust settings, and is idempotent", () => {
  const apt = mkdtempSync(join(tmpdir(), "agentic-apt-"));
  try {
    mkdirSync(join(apt, "sources.list.d"));
    mkdirSync(join(apt, "apt.conf.d"));
    const cases = [
      ["apt-mirrors.txt", "http://azure.archive.ubuntu.com/ubuntu\nhttp://archive.ubuntu.com/ubuntu\n", "http://archive.ubuntu.com/ubuntu\nhttp://archive.ubuntu.com/ubuntu\n"],
      ["apt-mirrors-security.txt", "http://security.ubuntu.com/ubuntu\n", "http://security.ubuntu.com/ubuntu\n"],
      ["sources.list", "deb mirror+file:/etc/apt/apt-mirrors.txt noble main\n", "deb mirror+file:/etc/apt/apt-mirrors.txt noble main\n"],
      ["sources.list.d/ubuntu.sources", "URIs: https://azure.archive.ubuntu.com/ubuntu\nSuites: noble noble-updates\nSigned-By: /usr/share/keyrings/ubuntu-archive-keyring.gpg\n", "URIs: https://archive.ubuntu.com/ubuntu\nSuites: noble noble-updates\nSigned-By: /usr/share/keyrings/ubuntu-archive-keyring.gpg\n"],
      ["sources.list.d/ports.list", "deb http://ports.ubuntu.com/ubuntu-ports noble main\n", "deb http://ports.ubuntu.com/ubuntu-ports noble main\n"],
      ["sources.list.d/other.list", "deb https://azure.archive.ubuntu.com.example/ubuntu noble main\n", "deb https://azure.archive.ubuntu.com.example/ubuntu noble main\n"],
      ["sources.list.d/backup.sources.disabled", "http://azure.archive.ubuntu.com/ubuntu", "http://azure.archive.ubuntu.com/ubuntu"],
    ];
    for (const [name, input] of cases) writeFileSync(join(apt, name), input);
    for (let pass = 0; pass < 2; pass++) {
      configureApt(apt);
      for (const [name, , expected] of cases) assert.equal(readFileSync(join(apt, name), "utf8"), expected, name);
      assert.equal(readFileSync(join(apt, "apt.conf.d/99-agentic-job-network"), "utf8"), NETWORK_CONFIG);
      for (const directive of ['Acquire::http::Timeout "10";', 'Acquire::https::Timeout "10";', 'Acquire::Retries "3";']) {
        assert.ok(NETWORK_CONFIG.includes(directive), directive);
      }
    }
    configureApt(join(apt, "missing"));
  } finally {
    rmSync(apt, { recursive: true, force: true });
  }
});

test("apt abandons a stalled response body at its data timeout", {
  skip: !existsSync("/usr/lib/apt/apt-helper"), timeout: 15000,
}, async () => {
  const dir = mkdtempSync(join(tmpdir(), "agentic-apt-stall-"));
  let bodyStarted = false;
  const server = createServer((_request, response) => {
    response.writeHead(200, { "Content-Length": "1000000" });
    response.write("partial body");
    bodyStarted = true;
  });
  try {
    await new Promise(resolve => server.listen(0, "127.0.0.1", resolve));
    const started = performance.now();
    const child = spawn("/usr/lib/apt/apt-helper", ["-o", "Acquire::http::Timeout=1",
      "-o", "Acquire::Retries=0", "download-file",
      `http://127.0.0.1:${server.address().port}/package.deb`, join(dir, "package.deb")],
    { stdio: "ignore", timeout: 10000 });
    const code = await new Promise((resolve, reject) => {
      child.on("error", reject);
      child.on("exit", resolve);
    });
    assert.equal(bodyStarted, true);
    assert.notEqual(code, null, "apt must exit itself, not be killed by the test watchdog");
    assert.notEqual(code, 0);
    assert.ok(performance.now() - started < 9000, "data timeout must interrupt a stalled body");
  } finally {
    server.closeAllConnections();
    await new Promise(resolve => server.close(resolve));
    rmSync(dir, { recursive: true, force: true });
  }
});
