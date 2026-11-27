#!/usr/bin/env python3
"""整理 cargo 产出的三个可执行文件为发布目录，并生成 SHA256SUMS。

用法:
    package.py --triple <rustc 目标三元组> --out <输出目录> --version X.Y.Z [--target-dir target]

输出目录内容: ai2048 / simulate / train（Windows 目标带 .exe）、SHA256SUMS、README.md、LICENSE。
每个可执行文件都必须非空且包含版本号字符串，防止把陈旧产物发布出去。

SHA256SUMS 与 sha256sum 工具的格式一致，可在类 Unix 平台用 `sha256sum -c SHA256SUMS` 校验。
"""

import argparse
import hashlib
import os
import shutil
import sys

BINS = ("ai2048", "simulate", "train")


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def main():
    if hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    ap = argparse.ArgumentParser(description="整理发布目录并生成 SHA256SUMS")
    ap.add_argument("--triple", required=True, help="rustc 目标三元组")
    ap.add_argument("--out", required=True, help="输出目录")
    ap.add_argument("--version", required=True, help="期望的版本号")
    ap.add_argument("--target-dir", default="target", help="cargo 的 target 目录")
    args = ap.parse_args()

    suffix = ".exe" if "windows" in args.triple else ""
    src_dir = os.path.join(args.target_dir, args.triple, "release")
    os.makedirs(args.out, exist_ok=True)
    names = []
    for b in BINS:
        src = os.path.join(src_dir, b + suffix)
        if not os.path.isfile(src):
            sys.exit("package: 找不到 %s（是否忘了 --target %s？）" % (src, args.triple))
        with open(src, "rb") as f:
            data = f.read()
        if not data:
            sys.exit("package: %s 是空文件" % src)
        if args.version.encode("ascii") not in data:
            sys.exit("package: %s 不包含版本号 %s（可能是陈旧产物）" % (src, args.version))
        dst = os.path.join(args.out, b + suffix)
        shutil.copy2(src, dst)
        if suffix == "":
            os.chmod(dst, 0o755)  # 归档/下载可能丢失可执行位，这里显式保证
        names.append(b + suffix)
        print("  %-12s %8d 字节  %s" % (b + suffix, len(data), sha256(dst)[:16] + "…"))

    repo_root = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
    for extra in ("README.md", "LICENSE"):
        src = os.path.join(repo_root, extra)
        if os.path.isfile(src):
            shutil.copy2(src, os.path.join(args.out, extra))
            names.append(extra)
        else:
            print("  （未找到 %s，未随包附带）" % extra)

    sums = os.path.join(args.out, "SHA256SUMS")
    with open(sums, "w", encoding="utf-8", newline="\n") as f:
        for n in sorted(names):
            f.write("%s  %s\n" % (sha256(os.path.join(args.out, n)), n))
    print("package: %s 就绪（%d 个文件，%s）" % (args.out, len(names) + 1, args.triple))


if __name__ == "__main__":
    main()
