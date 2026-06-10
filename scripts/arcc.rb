class Arcc < Formula
  desc "Three-in-One Personal AI Assistant (DeepSeek-V4)"
  homepage "https://github.com/niyongsheng/arcc"
  version "0.1.1"
  license "MIT"

  if OS.mac? && Hardware::CPU.arm?
    url "https://github.com/niyongsheng/arcc/releases/download/v0.1.1/arcc-aarch64-apple-darwin.tar.gz"
    sha256 "f92c0e8514008817cbf460a19546131c6cc4ea503cfc89ad4f161dee03c16e3e"
  elsif OS.linux? && Hardware::CPU.intel?
    url "https://github.com/niyongsheng/arcc/releases/download/v0.1.1/arcc-x86_64-unknown-linux-gnu.tar.gz"
    sha256 "e8916bda3a3edd7cb3eba3fd8d7bd5bfeae285c851755ab3bb328ea2fad079d4"
  else
    odie "Unsupported platform. Supported: macOS Apple Silicon, Linux x86_64"
  end

  def install
    bin.install "arcc"
  end

  test do
    assert_match "AI Rust Claude CLI", shell_output("#{bin}/arcc --help")
  end
end
