"""End-to-end checks for the public ZIP-to-source installer, without a Rust build."""

import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
import zipfile


INSTALLER = Path(__file__).resolve().parents[1] / "install.sh"


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


if __name__ == "__main__":
    unittest.main()
