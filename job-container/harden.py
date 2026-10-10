"""Remove ambient routes to root and fail closed on the final filesystem."""

import os
import stat
import subprocess


HELPERS = {
    "/usr/bin/newuidmap": "cap_setuid=ep",
    "/usr/bin/newgidmap": "cap_setgid=ep",
}


def files():
    for root, directories, names in os.walk("/", followlinks=False):
        # Kernel filesystems are not image contents (and are not writable).
        if root == "/":
            directories[:] = [d for d in directories if d not in {"proc", "sys", "dev"}]
        for name in names:
            path = os.path.join(root, name)
            info = os.lstat(path)
            if stat.S_ISREG(info.st_mode):
                yield path, info


def capabilities():
    result = subprocess.run(
        ["getcap", "-r", "/"], text=True, capture_output=True, check=True
    )
    return dict(line.split(" ", 1) for line in result.stdout.splitlines())


def main():
    for path, info in files():
        if info.st_mode & (stat.S_ISUID | stat.S_ISGID):
            os.chmod(path, stat.S_IMODE(info.st_mode) & ~0o6000)
    for path in capabilities():
        subprocess.run(["setcap", "-r", path], check=True)
    for path, capability in HELPERS.items():
        subprocess.run(["setcap", capability, path], check=True)

    audit()


def audit():
    privileged = [path for path, info in files() if info.st_mode & 0o6000]
    if privileged:
        raise SystemExit(f"FAIL: setuid/setgid files remain: {privileged}")
    found = capabilities()
    if found != HELPERS:
        raise SystemExit(f"FAIL: unexpected file capabilities: {found}")
    if any(os.path.exists(path) for path in ("/usr/bin/sudo", "/bin/sudo")):
        raise SystemExit("FAIL: sudo remains")
    print("PASS: no setuid/setgid files; only mapping helpers have capabilities")


if __name__ == "__main__":
    main()
