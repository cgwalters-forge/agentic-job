"""Exercise the fail-closed audit without changing the host filesystem."""

import stat
import types
import unittest
from unittest.mock import patch

import harden


class AuditTests(unittest.TestCase):
    def test_rejects_privileged_inventory(self):
        cases = [
            ("setuid", 0o4755, harden.HELPERS, False, "setuid/setgid"),
            ("setgid", 0o2755, harden.HELPERS, False, "setuid/setgid"),
            ("extra capability", 0, {**harden.HELPERS, "/unexpected": "cap_net_admin=ep"}, False, "unexpected file capabilities"),
            ("missing helper", 0, {}, False, "unexpected file capabilities"),
            ("wrong capability", 0, {**harden.HELPERS, "/usr/bin/newuidmap": "cap_setuid=eip"}, False, "unexpected file capabilities"),
            ("sudo", 0, harden.HELPERS, True, "sudo remains"),
        ]
        for name, mode, capabilities, sudo, message in cases:
            files = [("/unexpected", types.SimpleNamespace(st_mode=stat.S_IFREG | mode))]
            with (
                self.subTest(name=name),
                patch.object(harden, "files", return_value=files),
                patch.object(harden, "capabilities", return_value=capabilities),
                patch.object(harden.os.path, "exists", return_value=sudo),
            ):
                with self.assertRaisesRegex(SystemExit, message):
                    harden.audit()

    def test_accepts_exact_allowlist(self):
        with (
            patch.object(harden, "files", return_value=[]),
            patch.object(harden, "capabilities", return_value=harden.HELPERS),
            patch.object(harden.os.path, "exists", return_value=False),
        ):
            harden.audit()


if __name__ == "__main__":
    unittest.main()
