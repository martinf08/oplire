import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]


class SafeScriptTests(unittest.TestCase):
    def test_429_command_does_not_initialize_or_change_proxy_state(self):
        with tempfile.TemporaryDirectory() as state_dir:
            env = os.environ.copy()
            env["OPLIRE_STATE_DIR"] = state_dir
            result = subprocess.run(
                [sys.executable, str(ROOT / "bin" / "proxy-rotate"), "429"],
                check=False,
                capture_output=True,
                text=True,
                env=env,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("no proxy switch", result.stdout)
            self.assertFalse((Path(state_dir) / "proxies.db").exists())

            rotate = subprocess.run(
                [
                    sys.executable,
                    str(ROOT / "bin" / "proxy-rotate"),
                    "rotate",
                    "--reason",
                    "HTTP 429 quota",
                ],
                check=False,
                capture_output=True,
                text=True,
                env=env,
            )
            self.assertEqual(rotate.returncode, 2, rotate.stderr)
            self.assertIn("must never trigger", rotate.stdout)
            self.assertFalse((Path(state_dir) / "proxies.db").exists())

    def test_proxy_hunt_is_disabled(self):
        result = subprocess.run(
            ["sh", str(ROOT / "bin" / "proxy-hunt")],
            check=False,
            capture_output=True,
            text=True,
        )
        self.assertEqual(result.returncode, 2)
        self.assertIn("disabled", result.stderr)

    def test_reset_launcher_does_not_accept_legacy_rotation_flags(self):
        result = subprocess.run(
            ["sh", str(ROOT / "bin" / "reset-opencode"), "--default"],
            check=False,
            capture_output=True,
            text=True,
        )
        self.assertEqual(result.returncode, 2)
        self.assertIn("disabled", result.stderr)

    def test_reset_launcher_executes_opencode_without_a_test_request(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            call_log = directory / "args.txt"
            executable = directory / "opencode"
            executable.write_text(
                '#!/bin/sh\nprintf "%s\\n" "$@" > "$OPLIRE_TEST_CALL_LOG"\n',
                encoding="utf-8",
            )
            executable.chmod(0o755)
            env = os.environ.copy()
            env["PATH"] = str(directory) + os.pathsep + env.get("PATH", "")
            env["OPLIRE_TEST_CALL_LOG"] = str(call_log)

            result = subprocess.run(
                ["sh", str(ROOT / "bin" / "reset-opencode"), "--", "run", "hello"],
                check=False,
                capture_output=True,
                text=True,
                env=env,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(call_log.read_text(encoding="utf-8").splitlines(), ["--", "run", "hello"])


if __name__ == "__main__":
    unittest.main()
