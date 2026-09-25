"""Offline release-install regression tests; no Rust build or real home writes."""

import hashlib
import io
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest


INSTALLER = Path(__file__).resolve().parents[1] / "install.sh"


class BinaryInstallTests(unittest.TestCase):
    def install_fixture(self, *, runner=True):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        downloads = root / "downloads"
        downloads.mkdir()
        mocks = root / "mocks"
        mocks.mkdir()
        home = root / "home"
        home.mkdir()
        bindir = root / "bin"
        bindir.mkdir()
        calls = root / "calls"
        target = "aarch64-unknown-linux-gnu"
        asset = f"harw-v0.3.0-{target}.tar.gz"
        names = ["harw", "killer"] + (["harw-agent-runner"] if runner else [])
        with tarfile.open(downloads / asset, "w:gz") as archive:
            for name in names:
                body = b'#!/bin/sh\nprintf "%s\\n" "$*" >> "$INSTALL_TEST_CALLS"\n'
                entry = tarfile.TarInfo(f"release/{name}")
                entry.size = len(body)
                entry.mode = 0o755
                archive.addfile(entry, io.BytesIO(body))
        digest = hashlib.sha256((downloads / asset).read_bytes()).hexdigest()
        (downloads / "SHA256SUMS").write_text(f"{digest}  {asset}\n")
        for name, body in {
            "uname": '#!/bin/sh\ncase "$1" in -s) echo Linux;; -m) echo aarch64;; esac\n',
            "curl": '#!/bin/sh\ncp "$INSTALL_TEST_DOWNLOADS/${2##*/}" "$4"\n',
        }.items():
            path = mocks / name
            path.write_text(body)
            path.chmod(0o755)
        # Missing runner must not partly replace an existing installation.
        (bindir / "harw").write_text("previous installation\n")
        env = dict(os.environ, HOME=str(home), HARW_HOME=str(home / ".harw"),
                   HARW_INSTALL_DIR=str(bindir), HARW_REPO="fixture/harw",
                   INSTALL_TEST_CALLS=str(calls), INSTALL_TEST_DOWNLOADS=str(downloads),
                   PATH=f"{mocks}:/usr/bin:/bin")
        result = subprocess.run(
            ["bash", str(INSTALLER), "--binary", "--version", "v0.3.0"],
            env=env, capture_output=True, text=True, timeout=20,
        )
        return result, bindir, calls

    def test_binary_install_includes_runner_and_install_record(self):
        result, bindir, calls = self.install_fixture()
        self.assertEqual(result.returncode, 0, result.stderr)
        for name in ("harw", "killer", "harw-agent-runner"):
            self.assertTrue(os.access(bindir / name, os.X_OK), name)
        commands = calls.read_text().splitlines()
        self.assertIn(f"agent install-record --bindir {bindir}", commands)
        self.assertIn("agent auto-build-uia", commands)
        self.assertNotIn("--source-dir", calls.read_text())

    def test_missing_runner_fails_before_replacing_existing_install(self):
        result, bindir, calls = self.install_fixture(runner=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("did not contain a 'harw-agent-runner'", result.stderr)
        self.assertEqual((bindir / "harw").read_text(), "previous installation\n")
        self.assertFalse(calls.exists())


if __name__ == "__main__":
    unittest.main()
