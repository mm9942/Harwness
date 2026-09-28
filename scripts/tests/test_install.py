"""End-to-end checks for the public installer (mirror binary and ZIP-to-source), without a Rust build."""

import hashlib
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import unittest
import zipfile


INSTALLER = Path(__file__).resolve().parents[1] / "install.sh"
MANIFEST = Path(__file__).resolve().parents[1] / "release-manifest.sh"


class SourceInstallTests(unittest.TestCase):
    def test_piped_install_downloads_zip_and_keeps_source_for_make(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            home, mocks = root / "home", root / "mocks"
            home.mkdir()
            mocks.mkdir()
            (home / ".bashrc").write_text("# existing config\n")
            archive = root / "Harwness-main.zip"
            with zipfile.ZipFile(archive, "w") as z:
                z.writestr("Harwness-main/Cargo.toml", '[workspace]\n')
                z.writestr("Harwness-main/Makefile", 'install:\n\t@true\n')

            self.executable(mocks / "curl", """#!/bin/sh
printf '%s\\n' "$*" >> "$HARW_TEST_URLS"
for arg do
  if [ "$prev" = -o ]; then output=$arg; fi
  prev=$arg
done
case "$*" in
  *Harwness-main.zip*) printf 'zip\\n' >> "$HARW_TEST_EVENTS"; cp "$HARW_TEST_ARCHIVE" "$output" ;;
  *sh.rustup.rs*) cat > "$output" <<'RUSTUP'
#!/bin/sh
printf 'rustup\\n' >> "$HARW_TEST_EVENTS"
printf '%s\\n' "$*" >> "$HARW_TEST_RUSTUP"
mkdir -p "$HOME/.cargo/bin"
for name in cargo rustc rustup; do
  printf '#!/bin/sh\\nexit 0\\n' > "$HOME/.cargo/bin/$name"
  chmod +x "$HOME/.cargo/bin/$name"
done
RUSTUP
    ;;
  *) exit 1 ;;
esac
""")
            self.executable(mocks / "pkg-config", "#!/bin/sh\nexit 0\n")
            self.executable(mocks / "cmake", "#!/bin/sh\nexit 0\n")
            self.executable(mocks / "git", "#!/bin/sh\nexit 0\n")
            self.executable(mocks / "cc", "#!/bin/sh\nexit 0\n")
            self.executable(mocks / "bwrap", "#!/bin/sh\nexit 0\n")
            self.executable(mocks / "prlimit", "#!/bin/sh\nexit 0\n")
            self.executable(
                mocks / "unzip",
                '#!/bin/sh\nprintf "unzip\\n" >> "$HARW_TEST_EVENTS"\n'
                f'exec "{shutil.which("unzip")}" "$@"\n',
            )
            self.executable(mocks / "make", """#!/bin/sh
printf 'make\\n' >> "$HARW_TEST_EVENTS"
printf '%s|%s\\n' "$PWD" "$*" >> "$HARW_TEST_MAKE"
mkdir -p "$HARW_INSTALL_DIR"
printf '#!/bin/sh\\necho harw-test\\n' > "$HARW_INSTALL_DIR/harw"
chmod +x "$HARW_INSTALL_DIR/harw"
""")
            env = dict(
                os.environ,
                HOME=str(home),
                PATH=f"{mocks}:/usr/bin:/bin",
                HARW_INSTALL_DIR=str(home / ".local/bin"),
                HARW_SOURCES_DIR=str(home / "sources"),
                HARW_TEST_ARCHIVE=str(archive),
                HARW_TEST_URLS=str(root / "urls"),
                HARW_TEST_EVENTS=str(root / "events"),
                HARW_TEST_RUSTUP=str(root / "rustup"),
                HARW_TEST_MAKE=str(root / "make"),
                HARW_BASE_URL="https://mirror.example/harw",
            )
            result = subprocess.run(
                ["bash"], input=INSTALLER.read_text(), env=env,
                capture_output=True, text=True, timeout=30,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("https://mirror.example/harw/Harwness-main.zip", (root / "urls").read_text())
            self.assertIn("-y --default-toolchain stable --profile default", (root / "rustup").read_text())
            self.assertEqual((root / "events").read_text().splitlines(), ["zip", "unzip", "rustup", "make"])
            source, args = (root / "make").read_text().strip().split("|")
            self.assertEqual(args, f"install BINDIR={home / '.local/bin'}")
            self.assertTrue((Path(source) / "Cargo.toml").is_file())
            self.assertTrue((Path(source) / "Makefile").is_file())
            self.assertIn("added by harw installer", (home / ".bashrc").read_text())

    @staticmethod
    def executable(path, content):
        path.write_text(content)
        path.chmod(0o755)


# Serves files below $HARW_TEST_MIRROR for URLs under https://mirror.example/harw/
# and fails like `curl -f` on a 404 for everything else.
MIRROR_CURL = """#!/bin/sh
printf '%s\\n' "$*" >> "$HARW_TEST_URLS"
for arg do
  if [ "$prev" = -o ]; then output=$arg; fi
  case "$arg" in https://*) url=$arg ;; esac
  prev=$arg
done
case "$url" in
  https://mirror.example/harw/*)
    file="$HARW_TEST_MIRROR/${url#https://mirror.example/harw/}"
    [ -f "$file" ] || exit 22
    cp "$file" "$output" ;;
  *) exit 22 ;;
esac
"""


class MirrorBinaryInstallTests(unittest.TestCase):
    TAG = "v9.9.9"

    def test_piped_install_prefers_mirror_release_on_x86_64(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            self.publish(root, ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"])
            result, urls = self.run_installer(root, "x86_64")
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn(f"{self.TAG}/harw-{self.TAG}-x86_64-unknown-linux-gnu.tar.gz", urls)
            self.assertNotIn("Harwness-main.zip", urls)
            self.assertNotIn("github.com", urls)
            for binary in ("harw", "killer", "harw-agent-runner"):
                self.assertTrue((root / "home/.local/bin" / binary).is_file(), binary)
            self.assertIn("harw-test x86_64-unknown-linux-gnu", result.stdout)

    def test_piped_install_selects_the_aarch64_tarball(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            self.publish(root, ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"])
            result, urls = self.run_installer(root, "aarch64")
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn(f"harw-{self.TAG}-aarch64-unknown-linux-gnu.tar.gz", urls)
            self.assertNotIn("x86_64-unknown-linux-gnu.tar.gz", urls)
            self.assertIn("harw-test aarch64-unknown-linux-gnu", result.stdout)

    def test_missing_tarball_for_this_machine_falls_back_to_source(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            self.publish(root, ["x86_64-unknown-linux-gnu"])
            result, urls = self.run_installer(root, "aarch64")
            # No zip is published either: the source path must fail with a
            # clear message after the binary probe, not with a bare curl error.
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("building from source", result.stdout)
            self.assertIn("Harwness-main.zip", urls)
            self.assertIn("no source archive published there", result.stderr)

    def test_source_path_prefers_the_versioned_source_tarball(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            self.publish(root, ["x86_64-unknown-linux-gnu"], source=True)
            result, urls = self.run_installer(root, "aarch64")
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn(f"{self.TAG}/harwness-9.9.9-source.tar.gz", urls)
            self.assertNotIn("Harwness-main.zip", urls)
            source, args = (root / "make").read_text().strip().split("|")
            self.assertEqual(args, f"install BINDIR={root / 'home/.local/bin'}")
            self.assertTrue((Path(source) / "Cargo.toml").is_file())
            self.assertIn("harw-test source", result.stdout)

    def test_tampered_source_tarball_installs_nothing(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            self.publish(root, ["x86_64-unknown-linux-gnu"], source=True)
            tarball = root / "mirror" / self.TAG / "harwness-9.9.9-source.tar.gz"
            tarball.write_bytes(tarball.read_bytes() + b"tampered")
            result, _ = self.run_installer(root, "aarch64")
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("checksum of harwness-9.9.9-source.tar.gz", result.stderr)
            self.assertFalse((root / "make").exists())

    def test_mirror_version_json_names_the_tag_without_latest(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            self.publish(root, ["x86_64-unknown-linux-gnu"], source=True, manifest=True)
            manifest = json.loads((root / "mirror/version.json").read_text())
            self.assertEqual(manifest["tag"], self.TAG)
            self.assertEqual(manifest["version"], "9.9.9")
            self.assertEqual(
                manifest["targets"]["x86_64-unknown-linux-gnu"]["file"],
                f"{self.TAG}/harw-{self.TAG}-x86_64-unknown-linux-gnu.tar.gz",
            )
            self.assertEqual(manifest["source"]["file"], f"{self.TAG}/harwness-9.9.9-source.tar.gz")
            result, urls = self.run_installer(root, "x86_64")
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("mirror.example/harw/version.json", urls)
            self.assertNotIn("mirror.example/harw/latest", urls)
            self.assertIn("harw-test x86_64-unknown-linux-gnu", result.stdout)

    def test_install_seeds_local_version_json_once(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            self.publish(root, ["x86_64-unknown-linux-gnu"])
            result, _ = self.run_installer(root, "x86_64")
            self.assertEqual(result.returncode, 0, result.stderr)
            record = root / "home/.harw/version.json"
            info = json.loads(record.read_text())
            self.assertEqual(info["latest_version"], "9.9.9")
            self.assertIsNone(info["dismissed_version"])
            self.assertRegex(info["last_checked_at"], r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$")
            # A later install keeps an existing record (it may hold a dismissal).
            record.write_text('{"latest_version":"9.9.9","last_checked_at":"2026-01-01T00:00:00Z","dismissed_version":"9.9.9"}\n')
            result, _ = self.run_installer(root, "x86_64")
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(json.loads(record.read_text())["dismissed_version"], "9.9.9")

    def test_binary_flag_without_latest_fails_clearly(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            (root / "mirror").mkdir()
            result, _ = self.run_installer(root, "x86_64", "--binary")
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("missing https://mirror.example/harw/latest", result.stderr)

    def test_checksum_mismatch_installs_nothing(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            self.publish(root, ["x86_64-unknown-linux-gnu"])
            tarball = root / "mirror" / self.TAG / f"harw-{self.TAG}-x86_64-unknown-linux-gnu.tar.gz"
            tarball.write_bytes(tarball.read_bytes() + b"tampered")
            result, _ = self.run_installer(root, "x86_64")
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("checksum", result.stderr)
            self.assertFalse((root / "home/.local/bin/harw").exists())

    def publish(self, root, targets, source=False, manifest=False):
        """Writes the release.yml `mirror` layout for self.TAG below root/mirror.

        With manifest, the mirror names the tag only in version.json (written by
        scripts/release-manifest.sh), not in `latest`.
        """
        release = root / "mirror" / self.TAG
        release.mkdir(parents=True)
        if not manifest:
            (root / "mirror" / "latest").write_text(self.TAG + "\n")
        sums = []
        for target in targets:
            name = f"harw-{self.TAG}-{target}"
            data = io.BytesIO()
            with tarfile.open(fileobj=data, mode="w:gz") as tar:
                directory = tarfile.TarInfo(name)
                directory.type, directory.mode = tarfile.DIRTYPE, 0o755
                tar.addfile(directory)
                for binary in ("harw", "killer", "harw-agent-runner"):
                    body = (
                        '#!/bin/sh\n'
                        f'[ "$1" = --version ] && echo "harw-test {target}"\n'
                        'exit 0\n'
                    ).encode()
                    info = tarfile.TarInfo(f"{name}/{binary}")
                    info.size, info.mode = len(body), 0o755
                    tar.addfile(info, io.BytesIO(body))
            (release / f"{name}.tar.gz").write_bytes(data.getvalue())
            sums.append(f"{hashlib.sha256(data.getvalue()).hexdigest()}  {name}.tar.gz\n")
        if source:
            name = "harwness-9.9.9"
            data = io.BytesIO()
            with tarfile.open(fileobj=data, mode="w:gz") as tar:
                directory = tarfile.TarInfo(name)
                directory.type, directory.mode = tarfile.DIRTYPE, 0o755
                tar.addfile(directory)
                for member, body in (("Cargo.toml", b"[workspace]\n"), ("Makefile", b"install:\n\t@true\n")):
                    info = tarfile.TarInfo(f"{name}/{member}")
                    info.size, info.mode = len(body), 0o644
                    tar.addfile(info, io.BytesIO(body))
            (release / f"{name}-source.tar.gz").write_bytes(data.getvalue())
            sums.append(f"{hashlib.sha256(data.getvalue()).hexdigest()}  {name}-source.tar.gz\n")
        (release / "SHA256SUMS").write_text("".join(sums))
        if manifest:
            written = subprocess.run(
                ["bash", str(MANIFEST), self.TAG, str(release)],
                capture_output=True, text=True, check=True,
            )
            (root / "mirror" / "version.json").write_text(written.stdout)

    def run_installer(self, root, machine, *args):
        home, mocks = root / "home", root / "mocks"
        home.mkdir(exist_ok=True)
        mocks.mkdir(exist_ok=True)
        SourceInstallTests.executable(mocks / "curl", MIRROR_CURL)
        SourceInstallTests.executable(mocks / "uname", f"""#!/bin/sh
case "$1" in
  -m) echo {machine} ;;
  -o) echo GNU/Linux ;;
  *) echo Linux ;;
esac
""")
        # Source path: a present Rust toolchain and build tools, and a `make`
        # that records its call and installs a stub harw.
        cargo_bin = home / ".cargo/bin"
        cargo_bin.mkdir(parents=True, exist_ok=True)
        for name in ("cargo", "rustc", "rustup"):
            SourceInstallTests.executable(cargo_bin / name, "#!/bin/sh\nexit 0\n")
        for name in ("pkg-config", "cmake", "git", "cc", "bwrap", "prlimit"):
            SourceInstallTests.executable(mocks / name, "#!/bin/sh\nexit 0\n")
        SourceInstallTests.executable(mocks / "make", f"""#!/bin/sh
printf '%s|%s\\n' "$PWD" "$*" >> "{root / 'make'}"
mkdir -p "$HARW_INSTALL_DIR"
printf '#!/bin/sh\\necho harw-test source\\n' > "$HARW_INSTALL_DIR/harw"
chmod +x "$HARW_INSTALL_DIR/harw"
""")
        env = dict(
            os.environ,
            HOME=str(home),
            PATH=f"{mocks}:/usr/bin:/bin",
            HARW_HOME=str(home / ".harw"),
            HARW_INSTALL_DIR=str(home / ".local/bin"),
            HARW_SOURCES_DIR=str(home / "sources"),
            HARW_TEST_MIRROR=str(root / "mirror"),
            HARW_TEST_URLS=str(root / "urls"),
            HARW_BASE_URL="https://mirror.example/harw",
        )
        for key in ("HARW_RELEASES_URL", "HARW_RELEASE_TAG"):
            env.pop(key, None)
        result = subprocess.run(
            ["bash", "-s", "--", *args], input=INSTALLER.read_text(), env=env,
            capture_output=True, text=True, timeout=30,
        )
        urls = (root / "urls").read_text() if (root / "urls").exists() else ""
        return result, urls


if __name__ == "__main__":
    unittest.main()
