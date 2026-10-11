"""Probe same-uid access to systemd's trusted, out-of-view PAM handlers."""

import ctypes
import errno
import os
import signal


class IOVec(ctypes.Structure):
    _fields_ = [("base", ctypes.c_void_p), ("length", ctypes.c_size_t)]


lib = ctypes.CDLL(None, use_errno=True)
lib.ptrace.argtypes = [ctypes.c_uint, ctypes.c_int, ctypes.c_void_p, ctypes.c_void_p]
lib.ptrace.restype = ctypes.c_long
lib.process_vm_writev.argtypes = [
    ctypes.c_int, ctypes.POINTER(IOVec), ctypes.c_ulong,
    ctypes.POINTER(IOVec), ctypes.c_ulong, ctypes.c_ulong,
]
lib.process_vm_writev.restype = ctypes.c_ssize_t
byte = ctypes.c_char(b"x")
vector = IOVec(ctypes.addressof(byte), 1)


def write_memory(pid):
    return lib.process_vm_writev(pid, ctypes.byref(vector), 1,
                                 ctypes.byref(vector), 1, 0)


# A child is traceable under ptrace_scope=1. Its inherited allocation gives
# the write control a known valid address without changing any executable code.
child = os.fork()
if child == 0:
    while True:
        signal.pause()
try:
    assert write_memory(child) == 1, "process_vm_writev control failed"
    assert lib.ptrace(16, child, None, None) == 0, "ptrace attach control failed"
    os.waitpid(child, os.WUNTRACED)
    assert lib.ptrace(17, child, None, None) == 0, "ptrace detach control failed"
finally:
    os.kill(child, signal.SIGKILL)
    os.waitpid(child, 0)


def comm(pid):
    """The name of a process, or None where /proc does not show it to us."""
    try:
        with open(f"/proc/{pid}/comm") as stream:
            return stream.read().strip()
    except OSError:
        return None


def children(pid):
    """The children of a process /proc shows us, hidden ones included."""
    found = set()
    try:
        tasks = os.listdir(f"/proc/{pid}/task")
    except OSError:
        return found
    for task in tasks:
        try:
            with open(f"/proc/{pid}/task/{task}/children") as stream:
                found.update(int(child) for child in stream.read().split())
        except (OSError, ValueError):
            continue
    return found


def cgroup_members():
    """The processes of this session's and the user manager's control groups."""
    manager = f"user.slice/user-{os.getuid()}.slice/user@{os.getuid()}.service"
    groups = [manager, f"{manager}/init.scope"]
    try:
        with open("/proc/self/cgroup") as stream:
            groups += [line.strip()[3:].lstrip("/") for line in stream
                       if line.startswith("0::")]
    except OSError:
        pass
    found = set()
    for group in groups:
        try:
            with open(f"/sys/fs/cgroup/{group}/cgroup.procs") as stream:
                found.update(int(pid) for pid in stream.read().split())
        except (OSError, ValueError):
            continue
    return found


def is_ours(pid):
    """Whether a live process may be signalled by us: one of this uid."""
    try:
        os.kill(pid, 0)
    except OSError:
        return False
    return True


# A handler changes its credentials without exec, so it is nondumpable, and
# hidepid hides a nondumpable process even from its own uid. Where /proc shows
# it, find it by name; where not, it is the process of ours that /proc hides,
# known as a child of one it shows or by its control group.
handlers = set()
candidates = cgroup_members()
for name in os.listdir("/proc"):
    if not name.isdecimal():
        continue
    try:
        with open(f"/proc/{name}/status") as stream:
            fields = dict(line.split(":", 1) for line in stream if ":" in line)
        if int(fields["Uid"].split()[0]) != os.getuid():
            continue
    except (OSError, KeyError, ValueError):
        continue
    if comm(name) == "(sd-pam)":
        handlers.add(int(name))
    candidates |= children(name)
hidden = {pid for pid in candidates if pid > 0 and comm(pid) is None and is_ours(pid)}
handlers |= hidden

assert len(handlers) >= 2, f"expected both user-manager and run0 PAM handlers, found {sorted(handlers)}"
for pid in handlers:
    assert lib.ptrace(16, pid, None, None) == -1, f"traceable PAM handler {pid}"
    assert ctypes.get_errno() == errno.EPERM, "ptrace failed for an unrelated reason"
    assert write_memory(pid) == -1, f"writable PAM handler {pid}"
    assert ctypes.get_errno() == errno.EPERM, "memory write failed for an unrelated reason"
    for path in [f"/proc/{pid}/mem", f"/proc/{pid}/root/etc/agentic-job/write-probe/pam"]:
        try:
            fd = os.open(path, os.O_WRONLY | (os.O_CREAT if path.endswith("/pam") else 0), 0o600)
        except PermissionError:
            pass
        except FileNotFoundError:
            # Only as hidepid's answer for a process it hides.
            assert pid in hidden, f"{path} is missing for a visible PAM handler"
        else:
            os.close(fd)
            raise AssertionError(f"accessible PAM handler escape: {path}")
