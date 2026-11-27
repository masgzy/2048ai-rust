#!/usr/bin/env python3
"""把各目标的发布目录打包为压缩包，并生成总的 SHA256SUMS（Release 作业使用）。

用法:
    release_pack.py --dist <各目标目录所在目录> --version X.Y.Z --out <输出目录>

<dist> 下每个子目录是一个目标包（如 ai2048-linux_amd64）。非 Windows 目标打 tar.gz
（保留可执行位），Windows 目标打 zip。输出:
    <out>/ai2048-<version>-<name>.tar.gz 或 .zip
    <out>/SHA256SUMS
"""

import argparse
import hashlib
import os
import sys
import tarfile
import zipfile

BINS = {"ai2048", "simulate", "train", "ai2048.exe", "simulate.exe", "train.exe"}


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def normalize(info):
    """tar 成员属性归一化：固定属主与时间戳；目录与可执行文件 0755，其余 0644。"""
    info.uid = info.gid = 0
    info.uname = info.gname = ""
    info.mtime = 0
    if info.isdir() or os.path.basename(info.name) in BINS:
        info.mode = 0o755
    else:
        info.mode = 0o644
    return info


def pack(src_dir, arcname, dst):
    if dst.endswith(".zip"):
        with zipfile.ZipFile(dst, "w", zipfile.ZIP_DEFLATED) as z:
            for root, _, files in sorted(os.walk(src_dir)):
                for name in sorted(files):
                    full = os.path.join(root, name)
                    rel = os.path.relpath(full, src_dir).replace(os.sep, "/")
                    z.write(full, "%s/%s" % (arcname, rel))
    else:
        with tarfile.open(dst, "w:gz") as t:
            t.add(src_dir, arcname=arcname, recursive=True, filter=normalize)


def verify_sums(src, binary):
    """逐行校验包内 SHA256SUMS，且可执行文件必须名列其中；返回问题描述或 None。

    各目标的包目录里同名文件很多（ai2048 / SHA256SUMS ...），一旦下载环节把多个包合并到
    同一目录，文件就会互相覆盖；这里的校验能在发布之前拦住这种混包。
    """
    path = os.path.join(src, "SHA256SUMS")
    listed = set()
    with open(path, encoding="utf-8") as f:
        for raw in f:
            line = raw.strip()
            if not line:
                continue
            parts = line.split(None, 1)
            if len(parts) != 2 or len(parts[0]) != 64:
                return "SHA256SUMS 格式非法: %r" % line
            name = parts[1].lstrip("*").replace("\\", "/")
            if "/" in name or name in ("..", "."):
                return "SHA256SUMS 含有非法路径: %r" % name
            fp = os.path.join(src, name)
            if not os.path.isfile(fp):
                return "SHA256SUMS 列出的文件不存在: %s" % name
            if sha256(fp) != parts[0].lower():
                return "文件内容与 SHA256SUMS 不符（可能混入了其它目标的产物）: %s" % name
            listed.add(name)
    if binary not in listed:
        return "SHA256SUMS 未列出可执行文件 %s" % binary
    return None


def main():
    if hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    ap = argparse.ArgumentParser(description="打包各目标并生成总 SHA256SUMS")
    ap.add_argument("--dist", required=True, help="各目标目录所在目录")
    ap.add_argument("--version", required=True, help="版本号（用于压缩包文件名）")
    ap.add_argument("--out", required=True, help="输出目录")
    args = ap.parse_args()

    if not os.path.isdir(args.dist):
        sys.exit("release_pack: 目录不存在: %s" % args.dist)
    targets = sorted(
        d for d in os.listdir(args.dist) if os.path.isdir(os.path.join(args.dist, d))
    )
    if not targets:
        sys.exit("release_pack: %s 下没有任何目标目录" % args.dist)
    os.makedirs(args.out, exist_ok=True)
    archives = []
    for dirname in targets:
        # 构建产物目录名为 ai2048-<目标名>，压缩包名只保留目标名
        name = dirname[len("ai2048-"):] if dirname.startswith("ai2048-") else dirname
        src = os.path.join(args.dist, dirname)
        missing = [
            f
            for f in ("ai2048", "ai2048.exe", "SHA256SUMS")
            if not os.path.isfile(os.path.join(src, f))
        ]
        if len(missing) == 3:
            sys.exit("release_pack: %s 不是完整的目标包（缺 ai2048 与 SHA256SUMS）" % src)
        problem = verify_sums(src, "ai2048.exe" if os.path.isfile(os.path.join(src, "ai2048.exe")) else "ai2048")
        if problem:
            sys.exit("release_pack: %s: %s" % (src, problem))
        ext = "zip" if "windows" in name else "tar.gz"
        dst = os.path.join(args.out, "ai2048-%s-%s.%s" % (args.version, name, ext))
        pack(src, "ai2048-%s-%s" % (args.version, name), dst)
        archives.append(dst)
        print("  %s  %d 字节" % (os.path.basename(dst), os.path.getsize(dst)))

    sums = os.path.join(args.out, "SHA256SUMS")
    with open(sums, "w", encoding="utf-8", newline="\n") as f:
        for a in sorted(archives):
            f.write("%s  %s\n" % (sha256(a), os.path.basename(a)))
    print("release_pack: %d 个压缩包 + SHA256SUMS 已写入 %s" % (len(archives), args.out))


if __name__ == "__main__":
    main()
