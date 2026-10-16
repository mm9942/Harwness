# Homebrew formula template for Harwness.
#
# TODO before publishing:
#   - set `url` to a tagged source tarball (e.g. GitHub release archive)
#   - set `sha256` to its checksum (`shasum -a 256 <tarball>`)
#   - set the version tag
class Harw < Formula
  desc "Standalone Rust agent harness (harw)"
  homepage "https://github.com/your-org/harwness"
  url "https://example.invalid/harwness-0.2.0.tar.gz" # TODO
  sha256 "0000000000000000000000000000000000000000000000000000000000000000" # TODO
  license any_of: ["MIT", "Apache-2.0"]

  depends_on "rust" => :build

  def install
    system "cargo", "install", "--locked", "--path", "harw-cli", "--root", prefix
  end

  test do
    assert_match "harw", shell_output("#{bin}/harw --help")
  end
end
