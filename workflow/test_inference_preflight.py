import importlib.util
from pathlib import Path
import re
import shutil
import socket
import subprocess
import tempfile
import unittest
from unittest.mock import MagicMock

spec = importlib.util.spec_from_file_location(
    "preflight", Path(__file__).with_name("inference-preflight.py")
)
preflight = importlib.util.module_from_spec(spec)
spec.loader.exec_module(preflight)


class PreflightTests(unittest.TestCase):
    def test_workflow_order_and_trusted_binary(self):
        root = Path(__file__).resolve().parents[1]
        workflow = (root / ".github/workflows/agentic-job.yml").read_text()
        prepare = (root / "prepare/action.yml").read_text()
        run = (root / "run/action.yml").read_text()
        policy, agent = workflow.split("\n  agent:\n", 1)
        agent = agent.split("\n  notify:\n", 1)[0]
        self.assertIn('"$JOB_DIR/bin/agentic-job" config', policy)
        self.assertNotIn("CONFIG_BINARY", workflow)
        self.assertIn('sudo install -m 0755 "$JOB_DIR/agentic-job" /usr/local/bin/agentic-job', prepare)
        self.assertLess(prepare.index("artifact-ids:"), prepare.index("- name: Install the binary"))
        self.assertIn("uses: ./.agentic-job-source/prepare", agent)
        self.assertIn("uses: ./.agentic-job-source/run", agent)
        # Follow the composite calls in place: installation must precede
        # configuration/preflight, and execution must follow host hardening.
        expanded = agent.replace("uses: ./.agentic-job-source/prepare", prepare)
        expanded = expanded.replace("uses: ./.agentic-job-source/run", run)
        self.assertIn("env: *configuration-env", agent)
        self.assertIn('agentic-job config ${CONFIG:+--from "$CONFIG"}', agent)
        self.assertNotIn('"$JOB_DIR/agentic-job" config', agent)
        names = ["Install the binary", "Join the tailnet", "Write the configuration", "Check inference proxy reachability", "Secure the host", "Prove the host is secured", "Run the agent"]
        # The wrapper and composite both name their run step; use the inner
        # step, after the composite's sandbox check.
        positions = [expanded.rindex(f"- name: {name}") for name in names]
        self.assertEqual(positions, sorted(positions))
        self.assertIn("@agentclientprotocol/claude-agent-acp@0.88.0", policy)

    def test_agent_configuration_uses_installed_binary(self):
        workflow = (Path(__file__).resolve().parents[1] / ".github/workflows/agentic-job.yml").read_text()
        agent = workflow.split("\n  agent:\n", 1)[1]
        step = agent.split("- name: Write the configuration\n", 1)[1]
        body = step.split("        run: |\n", 1)[1]
        script = re.match(r"(?:          [^\n]*\n)+", body).group()
        script = "\n".join(line.removeprefix("          ") for line in script.splitlines())

        # Artifact downloads lose execute bits. Only the installed PATH copy
        # may be invoked; echo stands in for the binary to record its arguments.
        with tempfile.TemporaryDirectory(dir=Path.home()) as directory:
            root = Path(directory)
            installed = root / "bin"
            installed.mkdir()
            for binary, mode in [(root / "agentic-job", 0o644), (installed / "agentic-job", 0o755)]:
                shutil.copyfile("/bin/echo", binary)
                binary.chmod(mode)
            env = dict.fromkeys([
                "CONFIG", "MODEL", "CONFIG_REPO", "CONFIG_REF", "CONFIG_PATH",
                "URL", "REGISTER", "AUDIENCE", "TOKEN_FILE", "TIMEOUT",
                "MAX_REQUESTS", "MAX_TASKS", "BUDGET", "PACKAGES", "NPM", "SETUP",
            ], "")
            env.update(JOB_DIR=str(root), PATH=f"{installed}:/usr/bin:/bin", AGENT="fake")
            result = subprocess.run(
                ["bash", "-euo", "pipefail", "-c", script], env=env,
                capture_output=True, text=True, timeout=10,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertTrue((root / "config.toml").read_text().startswith("config --string agent.name=fake "))

    def test_proposals_skip_agent_configuration(self):
        workflow = (Path(__file__).resolve().parents[1] / ".github/workflows/agentic-job.yml").read_text()
        policy = workflow.split("\n  agent:\n", 1)[0]
        step = policy.split("- name: Write the configuration\n", 1)[1].split("- id: policy", 1)[0]
        self.assertIn("if: ${{ inputs.proposals-artifact == '' && steps.event.outputs.admitted != 'false' }}", step)

    def test_fake_agent_skips_proxy_step(self):
        workflow = (Path(__file__).resolve().parents[1] / ".github/workflows/agentic-job.yml").read_text()
        step = workflow.split("- name: Check inference proxy reachability\n", 1)[1].split("- name:", 1)[0]
        self.assertIn("if: ${{ inputs.agent != 'fake' }}", step)
        self.assertIn('run: python3 "$SOURCE_DIR/workflow/inference-preflight.py" "$JOB_DIR/config.toml"', step)

        # Even a fake agent configured with an unreachable proxy must not connect.
        for agent, runs in [("fake", False), ("claude", True), ("opencode", True), ("codex", True)]:
            with self.subTest(agent=agent):
                connect = MagicMock()
                if agent != "fake":
                    preflight.check({"inference": {"url": "http://proxy.invalid:8080"}}, connect)
                self.assertEqual(connect.call_count, int(runs))

    def test_addresses(self):
        for url, address in [
            ("http://100.64.0.1:8080/path", ("100.64.0.1", 8080)),
            ("https://proxy.example", ("proxy.example", 443)),
            ("http://[::1]", ("::1", 80)),
        ]:
            with self.subTest(url=url):
                connect = MagicMock()
                preflight.check({"inference": {"url": url}}, connect)
                connect.assert_called_once_with(address, timeout=10)
                connect.return_value.__exit__.assert_called_once()

    def test_no_proxy(self):
        connect = MagicMock()
        preflight.check({}, connect)
        connect.assert_not_called()

    def test_e2e_callers_do_not_need_a_proxy(self):
        ci = (Path(__file__).resolve().parents[1] / ".github/workflows/ci.yml").read_text()
        for name in [
            "e2e-full", "e2e-limit", "e2e-event", "e2e-analysis",
            "e2e-analysis-refused", "e2e-review",
        ]:
            with self.subTest(job=name):
                # Job bodies are indented at least four spaces.
                job = ci.split(f"\n  {name}:\n", 1)[1]
                job = re.split(r"\n  [^ ]", job, maxsplit=1)[0]
                self.assertIn("agent: fake", job)
                self.assertNotIn("inference-url:", job)

    def test_refusals(self):
        for url in ["file:///etc/passwd", "http://", "http://proxy:invalid"]:
            with self.subTest(url=url), self.assertRaises(ValueError):
                preflight.check({"inference": {"url": url}})
        for error in [ConnectionRefusedError(), socket.timeout(), socket.gaierror()]:
            with self.subTest(error=error), self.assertRaisesRegex(
                RuntimeError, "control failed: the runner cannot reach.*tailscale"
            ):
                preflight.check(
                    {"inference": {"url": "http://127.0.0.1:8080"}},
                    MagicMock(side_effect=error),
                )

    def test_real_connection(self):
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            listener.listen()
            port = listener.getsockname()[1]
            preflight.check({"inference": {"url": f"http://127.0.0.1:{port}"}})


if __name__ == "__main__":
    unittest.main()
