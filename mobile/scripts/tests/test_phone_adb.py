"""Exercise the phone profile copy boundary using a recording ADB executable."""

import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


HELPER = Path(__file__).resolve().parents[1] / "phone-adb"
PUBLIC_PROFILE = {
    "version": 1,
    "label": "Local test gateway",
    "endpoint": "127.0.0.1:7443",
    "server_node_id": "gateway-mobile",
    "server_public_key_hex": "12" * 1952,
}


class PhoneAdbTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        directory = Path(self.tmp.name)
        self.profile = directory / "profile.json"
        self.recording = directory / "recording.json"
        adb = directory / "adb"
        adb.write_text(
            """#!/usr/bin/env python3
import os
from pathlib import Path
import sys

operation = sys.argv[3]
if operation == "get-state":
    print("device")
elif operation == "push":
    original = os.environ.get("PHONE_TEST_REPLACE_PROFILE")
    if original:
        Path(original).write_text("secret written after validation")
    Path(os.environ["PHONE_TEST_ADB_RECORDING"]).write_bytes(Path(sys.argv[4]).read_bytes())
else:
    raise SystemExit("unexpected ADB command")
"""
        )
        adb.chmod(0o700)
        self.env = {
            **os.environ,
            "ADB": str(adb),
            "PHONE_TEST_ADB_RECORDING": str(self.recording),
        }

    def run_helper(self, *args, extra_env=None):
        return subprocess.run(
            [HELPER, "--serial", "test-serial", *args],
            env=self.env | (extra_env or {}),
            capture_output=True,
            text=True,
            timeout=10,
        )

    def test_push_only_copies_the_validated_contents(self):
        self.profile.write_text(json.dumps(PUBLIC_PROFILE))
        result = self.run_helper(
            "push-profile",
            self.profile,
            extra_env={"PHONE_TEST_REPLACE_PROFILE": str(self.profile)},
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        recorded = self.recording.read_text()
        self.assertNotIn("secret written after validation", recorded)
        self.assertEqual(json.loads(recorded), PUBLIC_PROFILE)

    def test_push_rejects_unknown_fields_duplicates_and_bad_pins(self):
        for content in [
            "[credentials]\napi_key = 'do not copy'",
            json.dumps(PUBLIC_PROFILE | {"api_key": "do not copy"}),
            json.dumps(PUBLIC_PROFILE | {"server_public_key_hex": "00"}),
            json.dumps(PUBLIC_PROFILE | {"version": True}),
            json.dumps(PUBLIC_PROFILE | {"endpoint": "[::1%invalid-scope]:7443"}),
            '{"version": 1, ' + json.dumps(PUBLIC_PROFILE)[1:],
            json.dumps(PUBLIC_PROFILE) + " " * 17000,
        ]:
            with self.subTest(content_length=len(content)):
                self.profile.write_text(content)
                result = self.run_helper("push-profile", self.profile)
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse(self.recording.exists())

    def test_push_rejects_symlink_fifo_and_other_writable_input(self):
        self.profile.write_text(json.dumps(PUBLIC_PROFILE))
        link = self.profile.with_name("symlink")
        link.symlink_to(self.profile)
        self.assertNotEqual(self.run_helper("push-profile", link).returncode, 0)
        fifo = self.profile.with_name("fifo")
        os.mkfifo(fifo, 0o600)
        self.assertNotEqual(self.run_helper("push-profile", fifo).returncode, 0)
        self.profile.chmod(0o666)
        self.assertNotEqual(self.run_helper("push-profile", self.profile).returncode, 0)
        self.assertFalse(self.recording.exists())

    def test_pull_preserves_existing_enrollment(self):
        self.profile.write_text("existing public enrollment")
        result = self.run_helper("pull-enrollment", self.profile)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.profile.read_text(), "existing public enrollment")


if __name__ == "__main__":
    unittest.main()
