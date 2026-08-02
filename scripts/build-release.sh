#!/usr/bin/env bash
# 构建可在任意 Linux（x86_64）上运行的单文件 release 二进制。
#
# 产物：target/x86_64-unknown-linux-musl/release/icbm
#   - 静态链接（musl，无 glibc 版本依赖，可在任意 Linux 发行版运行）
#   - 内嵌资源文件：
#       · favicon（assets/icbmlogo.png，core/src/net/status.rs include_bytes!）
#       · datarouter-switch 二进制（lib/datarouter build.rs 编译并以
#         include_bytes! 嵌入，启动时经 memfd 内存执行，零落盘）
#   - 不依赖外部命令（region 终止用 libc::kill，而非系统 kill）
#
# 依赖：rustup target add x86_64-unknown-linux-musl + musl-gcc
#   Fedora/RHEL: sudo dnf install -y musl-gcc
#   Debian/Ubuntu: sudo apt install -y musl-tools
set -euo pipefail
cd "$(dirname "$0")/.."

if ! command -v musl-gcc >/dev/null 2>&1; then
    echo "缺少 musl-gcc（静态链接 C 依赖需要）。" >&2
    echo "  Fedora/RHEL: sudo dnf install -y musl-gcc" >&2
    echo "  Debian/Ubuntu: sudo apt install -y musl-tools" >&2
    exit 1
fi
rustup target add x86_64-unknown-linux-musl >/dev/null 2>&1 || true

CC=musl-gcc \
RUSTFLAGS="-C linker=musl-gcc -C target-feature=+crt-static" \
cargo build --release --target x86_64-unknown-linux-musl

BIN="target/x86_64-unknown-linux-musl/release/icbm"
if ! ldd "$BIN" 2>&1 | grep -q "statically linked"; then
    echo "警告：产物并非静态链接（ldd 未报告 statically linked）" >&2
fi
echo "构建完成：$BIN"
