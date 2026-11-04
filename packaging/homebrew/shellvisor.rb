# Homebrew formula for Shellvisor.
#
# Lives in the tap repository github.com/avmg0/homebrew-tap as
# Formula/shellvisor.rb, so users can run:
#   brew install avmg0/tap/shellvisor
#
# After each release, set `version` and the four sha256 values from the
# release's SHA256SUMS file (scripts/brew-formula.sh prints them filled in).
class Shellvisor < Formula
  desc "Multi-pane terminal workspace: your real shell plus files, system, data flow and coding agents"
  homepage "https://github.com/avmg0/shellvisor"
  version "VERSION"
  license "Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/avmg0/shellvisor/releases/download/v#{version}/shellvisor-v#{version}-aarch64-apple-darwin.tar.gz"
      sha256 "SHA_AARCH64_APPLE_DARWIN"
    end
    on_intel do
      url "https://github.com/avmg0/shellvisor/releases/download/v#{version}/shellvisor-v#{version}-x86_64-apple-darwin.tar.gz"
      sha256 "SHA_X86_64_APPLE_DARWIN"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/avmg0/shellvisor/releases/download/v#{version}/shellvisor-v#{version}-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "SHA_AARCH64_UNKNOWN_LINUX_GNU"
    end
    on_intel do
      url "https://github.com/avmg0/shellvisor/releases/download/v#{version}/shellvisor-v#{version}-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "SHA_X86_64_UNKNOWN_LINUX_GNU"
    end
  end

  def install
    bin.install "shellvisor"
    # `sv` is the same program; inside Shellvisor it is on PATH already,
    # this makes `sv on` work from a plain terminal too.
    bin.install_symlink "shellvisor" => "sv"
  end

  test do
    assert_match "shellvisor #{version}", shell_output("#{bin}/shellvisor --version")
    assert_match "not running", shell_output("#{bin}/sv status")
  end
end
