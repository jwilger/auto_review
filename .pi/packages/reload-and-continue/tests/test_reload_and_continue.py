"""Run with python3 .pi/packages/reload-and-continue/tests/test_reload_and_continue.py."""

import errno
import fcntl
import json
import os
import pty
import select
import shutil
import struct
import subprocess
import tempfile
import termios
import time
import unittest
from pathlib import Path

TESTS = Path(__file__).resolve().parent
PACKAGE = TESTS.parent
PI_DIR = PACKAGE.parent.parent
TOOL = "reload_and_continue"
NEW_RESOURCE = "new_regression_resource"
CONTINUATION = "Resume item seventeen using new_regression_resource from .pi/extensions/new-resource.ts."
SECOND_CONTINUATION = "Use the second request instead."
INITIAL_PROMPT = "Exercise the project reload tool."
NEXT_PROMPT = "This is an unrelated manual prompt after abort."
PI_VERSION = "1.6.0"
TIMEOUT = 10


class ReloadAndContinueTest(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="pi-reload-test-")
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        self.project = root / "project"
        home = root / "home"
        home.mkdir()
        extensions = self.project / ".pi" / "extensions"
        extensions.mkdir(parents=True)
        shutil.copyfile(TESTS / "scripted-provider.ts", extensions / "scripted-provider.ts")
        settings = PI_DIR / "settings.json"
        # Before implementation there may be no settings or package entrypoint.
        if settings.is_file():
            shutil.copyfile(settings, self.project / ".pi" / "settings.json")
        shutil.copytree(
            PACKAGE, self.project / ".pi" / "packages" / PACKAGE.name,
            ignore=shutil.ignore_patterns("tests"),
        )
        pi = shutil.which(os.environ.get("PI_TEST_BIN", "pi"))
        if pi is None:
            self.fail(f"Put Pi {PI_VERSION} on PATH or set PI_TEST_BIN")
        self.env = {"PATH": os.environ["PATH"], "HOME": str(home), "TERM": "xterm-256color",
                    "PI_CODING_AGENT_DIR": str(home / "agent")}
        version = subprocess.check_output([pi, "--version"], text=True, timeout=TIMEOUT,
                                          cwd=self.project, env=self.env).strip()
        self.assertEqual(version, PI_VERSION, "Use the pinned Pi release for this regression")
        self.command = [pi, "--approve", "--offline", "--no-session", "--provider", "regression",
                        "--model", "script"]

    def trace(self):
        path = self.project / "trace.jsonl"
        if not path.exists():
            return []
        return [json.loads(line) for line in path.read_text().splitlines(keepends=True)
                if line.endswith("\n")]

    def run_tui(self, scenario):
        master, slave = pty.openpty()
        self.addCleanup(os.close, master)
        try:
            fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 160, 0, 0))
            process = subprocess.Popen(
                [*self.command, INITIAL_PROMPT], cwd=self.project,
                env={**self.env, "RELOAD_TEST_SCENARIO": scenario},
                stdin=slave, stdout=slave, stderr=slave, start_new_session=True,
            )
        finally:
            os.close(slave)
        output = bytearray()

        def wait_for(predicate, description):
            deadline = time.monotonic() + TIMEOUT
            while time.monotonic() < deadline:
                events = self.trace()
                if predicate(events):
                    return events
                if select.select([master], [], [], 0.05)[0]:
                    try:
                        output.extend(os.read(master, 65536))
                    except OSError as error:
                        if error.errno != errno.EIO:
                            raise
                            break  # PTY closed (the process may still be exiting).
            events = self.trace()
            if predicate(events):
                return events
            self.fail(f"Pi did not {description}. Trace: {events}\n"
                      f"Terminal output: {output[-4000:].decode(errors='replace')}")

        def settled(count):
            return wait_for(lambda events: sum(e["event"] == "settled" for e in events) >= count,
                            f"settle run {count}")

        try:
            events = wait_for(lambda events: any(e["event"] == "request" for e in events),
                              "make an initial scripted model request")
            initial = next(e for e in events if e["event"] == "request")
            self.assertIn(TOOL, initial["tools"], "Missing project-discovered reload_and_continue tool")
            self.assertNotIn(NEW_RESOURCE, initial["tools"])
            self.assertEqual(events[0], {"event": "start", "reason": "startup", "mode": "tui"})
            if scenario == "abort":
                settled(1)
                os.write(master, f"{NEXT_PROMPT}\r".encode())
            settled(2)
            os.write(master, b"/quit\r")
            # Keep draining until shutdown so the child can't block on terminal output.
            wait_for(lambda events: any(e.get("reason") == "quit" for e in events), "handle /quit")
            self.assertEqual(process.wait(timeout=TIMEOUT), 0)
        finally:
            if process.poll() is None:
                process.kill()
                process.wait()
        return self.trace()

    def assert_continuation(self, scenario, result_count):
        events = self.run_tui(scenario)
        self.assertEqual([(e["event"], e.get("reason")) for e in events], [
            ("start", "startup"), ("request", None), *[("result", None)] * result_count,
            ("settled", None), ("shutdown", "reload"), ("start", "reload"),
            ("request", None), ("settled", None), ("shutdown", "quit"),
        ])
        self.assertFalse(any(e.get("aborted") for e in events))
        resumed = [e for e in events if e["event"] == "request"][1]
        self.assertIn(NEW_RESOURCE, resumed["tools"])
        text = json.dumps(resumed["input"])
        self.assertIn(CONTINUATION, text)
        self.assertNotIn(SECOND_CONTINUATION, text)
        self.assertRegex(
            text.replace(CONTINUATION, ""),
            r"(?i)\b(?:verify|confirm|check)\b.*\b(?:loaded|available|present|active)\b.*\bbefore\b",
        )

    def test_reloads_before_exactly_one_automatic_continuation(self):
        self.assert_continuation("single", result_count=1)

    def test_first_request_wins_when_both_calls_share_a_batch(self):
        self.assert_continuation("batch", result_count=2)

    def test_abort_does_not_reload_or_replay_on_next_run(self):
        events = self.run_tui("abort")
        self.assertEqual([(e["event"], e.get("reason")) for e in events], [
            ("start", "startup"), ("request", None), ("result", None), ("settled", None),
            ("request", None), ("settled", None), ("shutdown", "quit"),
        ])
        self.assertEqual([e["aborted"] for e in events if e["event"] == "settled"], [True, False])
        later = [e for e in events if e["event"] == "request"][1]
        self.assertIn(NEXT_PROMPT, json.dumps(later["input"]))
        self.assertNotIn(NEW_RESOURCE, later["tools"])

    def test_rpc_rejects_reload_even_with_rpc_ui(self):
        # RPC UI availability must not be confused with a real interactive TUI.
        with tempfile.TemporaryFile() as output, subprocess.Popen(
            [*self.command, "--mode", "rpc"], cwd=self.project,
            env={**self.env, "RELOAD_TEST_SCENARIO": "rpc"},
            stdin=subprocess.PIPE, stdout=output, stderr=subprocess.STDOUT,
        ) as process:
            assert process.stdin is not None  # Requested stdin=PIPE.
            try:
                prompt = json.dumps({"type": "prompt", "message": INITIAL_PROMPT})
                process.stdin.write(f"{prompt}\n".encode())
                process.stdin.flush()
                status = process.wait(timeout=TIMEOUT)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
                output.seek(0)
                self.fail(f"RPC Pi did not exit. Trace: {self.trace()}\n"
                          f"RPC output: {output.read()[-4000:].decode(errors='replace')}")
            self.assertEqual(status, 0)
        events = self.trace()
        self.assertEqual(events[0], {"event": "start", "reason": "startup", "mode": "rpc"})
        requests = [e for e in events if e["event"] == "request"]
        self.assertTrue(requests)
        self.assertIn(TOOL, requests[0]["tools"])
        results = [e for e in events if e["event"] == "result"]
        self.assertEqual([e["isError"] for e in results], [True])
        self.assertIn("reload_and_continue requires interactive Pi.", json.dumps(results[0]["result"]))
        self.assertFalse(any(e.get("reason") == "reload" for e in events))
        for request in (e for e in events if e["event"] == "request"):
            self.assertNotIn(CONTINUATION, json.dumps(request["input"]))


if __name__ == "__main__":
    unittest.main()
