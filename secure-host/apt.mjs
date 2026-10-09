// Run as root before package installation, on a disposable CI host only.
import { existsSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

export const NETWORK_CONFIG = 'Acquire::http::Timeout "10";\nAcquire::https::Timeout "10";\nAcquire::Retries "3";\n';

export function configureApt(apt = "/etc/apt") {
  if (!existsSync(apt)) return; // dnf hosts need no apt configuration.
  const sources = join(apt, "sources.list.d");
  const files = ["apt-mirrors.txt", "apt-mirrors-security.txt", "sources.list"];
  if (existsSync(sources)) {
    files.push(...readdirSync(sources).filter(name => /\.(list|sources)$/.test(name))
      .map(name => join("sources.list.d", name)));
  }
  for (const name of files) {
    const path = join(apt, name);
    if (!existsSync(path)) continue;
    const old = readFileSync(path, "utf8");
    // Keep mirror+file fallback and all suites, components and signing settings.
    // Replacing, rather than reordering, avoids mirror-method random selection.
    const updated = old.replace(/\b(https?:\/\/)azure\.archive\.ubuntu\.com(?=\/|\s|$)/g, "$1archive.ubuntu.com");
    if (updated !== old) writeFileSync(path, updated);
  }
  writeFileSync(join(apt, "apt.conf.d", "99-agentic-job-network"), NETWORK_CONFIG);
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    configureApt();
  } catch (error) {
    console.error(`configuring apt for CI: ${error.message}`);
    process.exitCode = 1;
  }
}
