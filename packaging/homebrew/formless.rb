class Formless < Formula
  desc "Tool-building AI agent harness: the agent builds the tools a task needs"
  homepage "https://formless.athiwat.xyz"
  url "https://github.com/athiwatc/formless/archive/refs/tags/v0.1.0.tar.gz"
  # Fill this from the GitHub release asset when bumping versions:
  #   curl -sL <url> | shasum -a 256
  sha256 "0000000000000000000000000000000000000000000000000000000000000000"
  license "Apache-2.0"

  depends_on "rust" => :build

  # Linux only: keyring's sync-secret-service backend links libdbus.
  on_linux do
    depends_on "dbus"
    depends_on "pkgconf" => :build
  end

  def install
    system "cargo", "install", *std_cargo_args(path: "crates/formless")
    # `formless completions bash|zsh|fish` prints each script.
    generate_completions_from_executable(bin/"formless", "completions")
    (man1/"formless.1").write Utils.safe_popen_read(bin/"formless", "man")
  end

  test do
    assert_match "formless", shell_output("#{bin}/formless --version")
  end
end
