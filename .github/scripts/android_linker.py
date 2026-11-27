#!/usr/bin/env python3
"""定位 Android NDK 的 clang 封装器，并通过 GITHUB_ENV 导出 cargo 链接器配置。

用法:
    python3 android_linker.py <rustc 目标三元组>

设置的环境变量（供后续 cargo build 步骤使用）:
    CARGO_TARGET_<TRIPLE>_LINKER=<NDK>/toolchains/llvm/prebuilt/<host>/bin/<arch><api>-clang
    RUSTFLAGS=-C link-arg=-Wl,-z,max-page-size=16384
    ANDROID_NDK_ROOT=<NDK>

NDK 候选目录按以下顺序取自 ANDROID_NDK / ANDROID_NDK_ROOT / ANDROID_NDK_HOME /
ANDROID_NDK_LATEST_HOME，其后是 $ANDROID_HOME/ndk/* 与 $ANDROID_SDK_ROOT/ndk/*（版本高的优先）。
取第一个**含有所需封装器**的候选：runner 上常同时装有稳定版与最新版 NDK，新版可能已放弃
某个架构，这时应顺延而不是直接失败。
16KB 页对齐是 Android 15+ 设备加载 ELF 的硬性要求（NDK r27 及更早默认仍是 4KB）。
"""

import glob
import os
import sys

# rustc 三元组 -> NDK clang 封装器前缀
WRAPPERS = {
    "aarch64-linux-android": "aarch64-linux-android",
    "armv7-linux-androideabi": "armv7a-linux-androideabi",
    "i686-linux-android": "i686-linux-android",
    "x86_64-linux-android": "x86_64-linux-android",
}
API = 21  # Android 5.0，NDK 支持的最低 API 级别


def ndk_version(path):
    """由目录名（如 27.3.13750724）得到可比较的版本号列表；非纯数字段按 0 计。"""
    return [int(p) if p.isdigit() else 0 for p in os.path.basename(path).split(".")]


def linker_pattern(triple):
    return os.path.join(
        "toolchains", "llvm", "prebuilt", "*", "bin", "%s%d-clang" % (WRAPPERS[triple], API)
    )


def find_linker(triple):
    """返回 (封装器路径, NDK 根目录)；找不到则返回 (None, 已检查的候选列表)。"""
    candidates = []  # NDK 根目录，按优先级排列
    for var in ("ANDROID_NDK", "ANDROID_NDK_ROOT", "ANDROID_NDK_HOME", "ANDROID_NDK_LATEST_HOME"):
        v = os.environ.get(var)
        if v:
            candidates.append(v)
    for var in ("ANDROID_HOME", "ANDROID_SDK_ROOT"):
        v = os.environ.get(var)
        if v:
            candidates += sorted(
                glob.glob(os.path.join(v, "ndk", "*")), key=ndk_version, reverse=True
            )

    tried = []
    seen = set()
    for ndk in candidates:
        if ndk in seen:
            continue
        seen.add(ndk)
        hits = sorted(glob.glob(os.path.join(ndk, linker_pattern(triple))))
        if hits:
            return hits[0], ndk
        tried.append(ndk)
    return None, tried


def main():
    if len(sys.argv) != 2 or sys.argv[1] not in WRAPPERS:
        sys.exit("用法: android_linker.py <%s>" % "|".join(sorted(WRAPPERS)))
    triple = sys.argv[1]
    env_file = os.environ.get("GITHUB_ENV")
    if not env_file:
        sys.exit("android_linker: 未设置 GITHUB_ENV（该脚本只在 GitHub Actions 步骤内使用）")

    linker, ndk = find_linker(triple)
    if not linker:
        if not ndk:
            sys.exit(
                "android_linker: 找不到 Android NDK（请设置 ANDROID_NDK 或 ANDROID_HOME；"
                "runner 镜像通常已预置）"
            )
        sys.exit(
            "android_linker: 找不到 %s（已检查的 NDK: %s；可用 ANDROID_NDK 指定其它 NDK 根目录）"
            % (linker_pattern(triple), "，".join(ndk))
        )

    env_var = "CARGO_TARGET_" + triple.upper().replace("-", "_") + "_LINKER"
    with open(env_file, "a", encoding="utf-8") as f:
        f.write("%s=%s\n" % (env_var, linker))
        f.write("RUSTFLAGS=-C link-arg=-Wl,-z,max-page-size=16384\n")
        f.write("ANDROID_NDK_ROOT=%s\n" % ndk)
    print("NDK:    %s" % ndk)
    print("linker: %s" % linker)
    print("%s=%s" % (env_var, linker))


if __name__ == "__main__":
    main()
