"""Run trusted analyzers on changed PR Git blobs, never PR programs."""

import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile


def git(repo, *args):
    return subprocess.run(["git", "--literal-pathspecs", "-C", str(repo), *args], check=True,
                          capture_output=True).stdout


def check(repo, base, head):
    if not all(re.fullmatch(r"[0-9a-f]{40}", ref) for ref in (base, head)):
        raise ValueError("base and head must be full commit SHAs")
    ancestor = git(repo, "merge-base", base, head).decode().strip()
    names = git(repo, "diff", "--no-ext-diff", "--no-renames", "--name-only",
                "-z", ancestor, head, "--").decode().split("\0")
    changed = [name for name in names if name.startswith(".github/")
               or Path(name).name in ("action.yml", "action.yaml")]
    if not changed:
        return True
    failed = False
    with tempfile.TemporaryDirectory(dir=os.environ.get("RUNNER_TEMP", Path.home())) as temp:
        for name in changed:
            if not (re.fullmatch(r"\.github/workflows/[^/]+\.ya?ml", name)
                    or Path(name).name in ("action.yml", "action.yaml")):
                continue
            entry = git(repo, "ls-tree", "-z", head, "--", name)
            if not entry:  # Deleted definitions have nothing to lint.
                continue
            mode, kind, oid = entry.split(b"\t", 1)[0].split()
            if mode not in (b"100644", b"100755") or kind != b"blob":
                raise ValueError(f"not a regular workflow/action: {name!r}")
            if int(git(repo, "cat-file", "-s", oid.decode())) > 1024 * 1024:
                raise ValueError(f"workflow/action exceeds 1 MiB: {name!r}")
            source = Path(temp) / Path(name).name
            source.write_bytes(git(repo, "cat-file", "blob", oid.decode()))
            print(f"Checking {name!r}", flush=True)
            commands = [[os.environ.get("ZIZMOR", "zizmor"), "--offline", "--no-config",
                         "--strict-collection", "--no-progress", str(source)]]
            if name.startswith(".github/workflows/"):
                commands.append([os.environ.get("ACTIONLINT", "actionlint"),
                                 "-config-file", str(Path(__file__).resolve().with_name("actionlint.yaml")),
                                 "-shellcheck=", "-pyflakes=", str(source)])
            for command in commands:
                result = subprocess.run(command, cwd=temp, capture_output=True)
                # Quote hostile source text so it cannot become workflow commands.
                print(json.dumps({"file": name, "stdout": result.stdout.decode(errors="replace"),
                                  "stderr": result.stderr.decode(errors="replace")}))
                failed |= result.returncode != 0
            source.unlink()
    return not failed


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("repo", type=Path)
    parser.add_argument("base")
    parser.add_argument("head")
    args = parser.parse_args()
    try:
        raise SystemExit(0 if check(args.repo, args.base, args.head) else 1)
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"Ingress failed: {error}\n")
