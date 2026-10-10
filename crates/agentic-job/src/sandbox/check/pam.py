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

handlers = []
for name in os.listdir("/proc"):
    if not name.isdecimal():
        continue
    try:
        with open(f"/proc/{name}/comm") as stream:
            comm = stream.read().strip()
        with open(f"/proc/{name}/status") as stream:
            fields = dict(line.split(":", 1) for line in stream if ":" in line)
        if comm == "(sd-pam)" and int(fields["Uid"].split()[0]) == os.getuid():
            handlers.append(int(name))
    except FileNotFoundError:
        continue

assert len(handlers) >= 2, "expected both user-manager and run0 PAM handlers"
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
        else:
            os.close(fd)
            raise AssertionError(f"accessible PAM handler escape: {path}")
