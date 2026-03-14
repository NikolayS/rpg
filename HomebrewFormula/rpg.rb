class Rpg < Formula
  desc "Self-driving Postgres agent and psql-compatible terminal"
  homepage "https://github.com/NikolayS/project-alpha"
  license "Apache-2.0"
  version "0.2.0"

  on_macos do
    if Hardware::CPU.intel?
      url "https://github.com/NikolayS/project-alpha/releases/download/v#{version}/rpg-x86_64-apple-darwin.tar.gz"
      # TODO: replace with actual sha256 once release artifacts are published
      sha256 "TODO_REPLACE_WITH_ACTUAL_SHA256"
    end
    if Hardware::CPU.arm?
      url "https://github.com/NikolayS/project-alpha/releases/download/v#{version}/rpg-aarch64-apple-darwin.tar.gz"
      # TODO: replace with actual sha256 once release artifacts are published
      sha256 "TODO_REPLACE_WITH_ACTUAL_SHA256"
    end
  end

  on_linux do
    if Hardware::CPU.intel?
      url "https://github.com/NikolayS/project-alpha/releases/download/v#{version}/rpg-x86_64-unknown-linux-gnu.tar.gz"
      # TODO: replace with actual sha256 once release artifacts are published
      sha256 "TODO_REPLACE_WITH_ACTUAL_SHA256"
    end
    if Hardware::CPU.arm?
      url "https://github.com/NikolayS/project-alpha/releases/download/v#{version}/rpg-aarch64-unknown-linux-gnu.tar.gz"
      # TODO: replace with actual sha256 once release artifacts are published
      sha256 "TODO_REPLACE_WITH_ACTUAL_SHA256"
    end
  end

  def install
    bin.install "rpg"
  end

  def caveats
    <<~EOS
      Configuration is stored in ~/.config/rpg/
    EOS
  end

  test do
    assert_match "rpg", shell_output("#{bin}/rpg --version")
  end
end
