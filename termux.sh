#!/data/data/com.termux/files/usr/bin/bash
# ai2048 —— Termux（Android）一键启动/构建脚本  v2.0.1
#
# 2026-11 已知问题：Termux 官方 rust 包当前缺少 std 静态库（rlib），
# 本地工程 cargo build 会报一串
#   "crate `std` required to be available in rlib format, but was not found in this form"
# 这是 Termux 打包缺陷（termux-packages issue #31547），与本项目无关。
#
# 因此本脚本的行为：
#   默认      —— 直接运行 prebuilt/ 下的预编译静态二进制（无需任何 Rust 环境）
#   --build   —— 走源码编译（有 rustup 用 rustup；检测到 rlib 报错时给出指引）
set -e
cd "$(dirname "$0")"

PORT="${PORT:-8080}"
ARCH="$(uname -m)"
PREBUILT="prebuilt/ai2048-android-$ARCH"

if [ "${1:-}" = "--build" ]; then
  echo "==> 源码编译模式"
  if [ -x "$HOME/.cargo/bin/rustc" ]; then
    # 关键：rustup 的 rustc 必须排在 $PREFIX/bin 里 Termux 自带 rustc 之前
    export PATH="$HOME/.cargo/bin:$PATH"
    echo "    使用 rustup 工具链：$("$HOME/.cargo/bin/rustc" --version)"
  else
    echo "    未检测到 rustup，使用 Termux 自带 rust（若报 rlib 错误请见 README 方式二）"
    pkg install -y rust
  fi
  pkg install -y clang binutils 2>/dev/null || true
  echo "==> 构建 release（aarch64 原生优化，首次约 1~2 分钟）"
  if ! RUSTFLAGS="-C target-cpu=native" cargo build --release --bin ai2048; then
    echo ""
    echo "!! 构建失败。若错误信息是 'required to be available in rlib format'，"
    echo "!! 这是 Termux rust 包的已知缺陷（termux-packages #31547），不是代码问题。"
    echo "!! 解决办法二选一："
    echo "!!   1) 直接运行预编译版： bash termux.sh   （无需 Rust）"
    echo "!!   2) 安装 rustup 后重试： 见 README.md『Termux 部署·方式二』"
    exit 1
  fi
  BIN="target/release/ai2048"
elif [ -f "$PREBUILT" ]; then
  echo "==> 使用预编译静态二进制 $PREBUILT（无需 Rust 环境）"
  chmod +x "$PREBUILT"
  BIN="$PREBUILT"
else
  echo "==> 未找到预编译二进制 $PREBUILT，回退到源码编译..."
  exec "$0" --build
fi

echo "==> 申请休眠锁（防止息屏后系统杀进程）"
termux-wake-lock 2>/dev/null || true

echo "==> 启动：手机浏览器打开 http://127.0.0.1:$PORT/"
echo "    局域网访问：ifconfig wlan0 查看手机 IP，电脑打开 http://<手机IP>:$PORT/"
echo "    停止：按 Ctrl+C，或运行 termux-wake-unlock"
exec "$BIN" --port "$PORT"
