class Formless < Formula
  desc "A tool-building AI agent harness: define a task, and the agent builds the tools it needs"
  homepage "https://formless.athiwat.xyz"
  url "https://github.com/athiwatc/formless/archive/refs/tags/v0.1.0.tar.gz"
  # Fill this from the GitHub release asset when bumping versions:
  #   curl -sL <url> | shasum -a 256
  sha256 "0000000000000000000000000000000000000000000000000000000000000000"
  license "Apache-2.0"

  depends_on "rust" => :build

  def install
    system "cargo", "install", *std_cargo_args(path: "crates/formless")
  end

  test do
    assert_match "formless", shell_output("#{bin}/formless --version")
  end
end
