#!/usr/bin/env python3
"""ELF 可执行文件校验（仅标准库；Linux/Windows/macOS runner 自带的 Python 3 均可运行）。

用法:
    check_elf.py --triple <rustc 目标三元组> <可执行文件> [更多可执行文件...]

根据三元组推断并逐项校验:
  - 体系结构 (e_machine)、ELF class (32/64 位)、字节序;
  - *-android : 必须是 PIE 动态可执行文件（ET_DYN + PT_INTERP 指向 /system/bin/linker*），
                且所有 PT_LOAD 段对齐 >= 16KB（否则在 Android 15+ 的 16KB 页设备上无法加载）;
  - *-musl    : 必须是纯静态链接（无 PT_INTERP；静态 PIE 形态允许带 PT_DYNAMIC）;
  - 其它 linux 目标: 只校验体系结构、位数与字节序。

退出码 0 表示全部通过。
"""

import argparse
import os
import struct
import sys

PT_LOAD, PT_DYNAMIC, PT_INTERP = 1, 2, 3
ET_EXEC, ET_DYN = 2, 3
ANDROID_PAGE = 16 * 1024

# 三元组的体系结构前缀 -> (e_machine, 是否 64 位, 是否小端)
ARCH = {
    "aarch64": (183, True, True),  # EM_AARCH64
    "x86_64": (62, True, True),    # EM_X86_64
    "i686": (3, False, True),      # EM_386
    "armv7": (40, False, True),    # EM_ARM
    "armv5te": (40, False, True),
    "arm": (40, False, True),
    "riscv64": (243, True, True),  # EM_RISCV
    "mips64el": (8, True, True),   # EM_MIPS（小端）
    "mips64": (8, True, False),    # EM_MIPS（大端）
    "mipsel": (8, False, True),
    "mips": (8, False, False),
}


def expectations(triple):
    """由三元组得到 (e_machine, 是否 64 位, 是否小端, 链接形态)。"""
    fields = triple.split("-")
    arch = fields[0]
    if arch not in ARCH:
        sys.exit("check_elf: 未知的目标体系结构: %s" % triple)
    if triple.endswith("-android"):
        kind = "android"
    elif any("musl" in f for f in fields[2:]):
        kind = "static"
    else:
        kind = "generic"
    machine, is64, little = ARCH[arch]
    return machine, is64, little, kind


def parse(path):
    """解析 ELF 头与程序头，返回关键属性。"""
    with open(path, "rb") as f:
        d = f.read()
    if len(d) < 16 or d[:4] != b"\x7fELF":
        raise ValueError("%s: 不是 ELF 文件" % path)
    ei_class, ei_data = d[4], d[5]
    if ei_class not in (1, 2) or ei_data not in (1, 2):
        raise ValueError("%s: 非法的 ELF ident" % path)
    is64 = ei_class == 2
    e = "<" if ei_data == 1 else ">"
    e_type, e_machine = struct.unpack_from(e + "HH", d, 16)
    if is64:
        (e_phoff,) = struct.unpack_from(e + "Q", d, 32)
        e_phentsize, e_phnum = struct.unpack_from(e + "HH", d, 54)
        fmt = e + "IIQQQQQQ"  # type flags offset vaddr paddr filesz memsz align
    else:
        (e_phoff,) = struct.unpack_from(e + "I", d, 28)
        e_phentsize, e_phnum = struct.unpack_from(e + "HH", d, 42)
        fmt = e + "IIIIIIII"  # type offset vaddr paddr filesz memsz flags align
    if e_phnum == 0 or e_phoff == 0:
        raise ValueError("%s: 没有程序头" % path)
    segs = []
    for i in range(e_phnum):
        v = struct.unpack_from(fmt, d, e_phoff + i * e_phentsize)
        if is64:
            p_type, _flags, p_offset, _vaddr, _paddr, p_filesz, p_memsz, p_align = v
        else:
            p_type, p_offset, _vaddr, _paddr, p_filesz, p_memsz, _flags, p_align = v
        segs.append((p_type, p_offset, p_filesz, p_memsz, p_align))
    interp = None
    for p_type, p_offset, p_filesz, _memsz, _align in segs:
        if p_type == PT_INTERP:
            interp = d[p_offset : p_offset + p_filesz].split(b"\0")[0]
            interp = interp.decode("ascii", "replace")
    return {
        "is64": is64,
        "little": ei_data == 1,
        "e_type": e_type,
        "e_machine": e_machine,
        "segs": segs,
        "interp": interp,
        "size": len(d),
    }


def check(path, machine, is64, little, kind):
    """返回 (问题列表, 摘要)。"""
    try:
        elf = parse(path)
    except (ValueError, struct.error) as exc:
        return [str(exc)], None
    problems = []
    if elf["e_machine"] != machine:
        problems.append("e_machine=%d，期望 %d" % (elf["e_machine"], machine))
    if elf["is64"] != is64:
        problems.append("应为 %d 位 ELF" % (64 if is64 else 32))
    if elf["little"] != little:
        problems.append("字节序不符（期望%s端）" % ("小" if little else "大"))
    if elf["e_type"] not in (ET_EXEC, ET_DYN):
        problems.append("不是可执行文件 (e_type=%d)" % elf["e_type"])
    kinds = [s[0] for s in elf["segs"]]
    has_interp, has_dyn = PT_INTERP in kinds, PT_DYNAMIC in kinds
    aligns = [s[4] for s in elf["segs"] if s[0] == PT_LOAD and s[3] > 0]
    min_align = min(aligns) if aligns else 0
    if kind == "static":
        # 判据是"没有动态加载器"（无 PT_INTERP）。静态 PIE（ET_DYN + PT_DYNAMIC，自重定位）
        # 同样是纯静态，musl 目标在不同架构/链接器下可能产出这两种形态之一。
        if has_interp:
            problems.append("应为纯静态链接，却需要动态加载器 %s" % elf["interp"])
    elif kind == "android":
        if elf["e_type"] != ET_DYN or not has_interp:
            problems.append("应为 PIE 动态可执行文件（ET_DYN + PT_INTERP）")
        elif not elf["interp"].startswith("/system/bin/linker"):
            problems.append("解释器应为 /system/bin/linker*，实际为 %s" % elf["interp"])
        if min_align < ANDROID_PAGE:
            problems.append(
                "PT_LOAD 最小对齐 0x%x < 0x%x（16KB 页设备无法加载）" % (min_align, ANDROID_PAGE)
            )
    if has_interp:
        linkage = "动态"
    elif has_dyn:
        linkage = "静态PIE"
    else:
        linkage = "静态"
    summary = "%s %s %s %s %s %d 字节 load对齐=0x%x 解释器=%s" % (
        "ELF64" if elf["is64"] else "ELF32",
        "LE" if elf["little"] else "BE",
        "e_machine=%d" % elf["e_machine"],
        {ET_EXEC: "EXEC", ET_DYN: "DYN(PIE)"}.get(elf["e_type"], "type=%d" % elf["e_type"]),
        linkage,
        elf["size"],
        min_align,
        elf["interp"] or "无",
    )
    return problems, summary


def main():
    if hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    ap = argparse.ArgumentParser(description="ELF 可执行文件校验")
    ap.add_argument("--triple", required=True, help="rustc 目标三元组，如 aarch64-linux-android")
    ap.add_argument("files", nargs="+", help="待校验的可执行文件")
    args = ap.parse_args()

    machine, is64, little, kind = expectations(args.triple)
    failed = 0
    for path in args.files:
        if not os.path.isfile(path):
            print("FAIL  %s: 文件不存在" % path)
            failed += 1
            continue
        problems, summary = check(path, machine, is64, little, kind)
        if problems:
            print("FAIL  %s: %s" % (path, "；".join(problems)))
            failed += 1
        else:
            print("OK    %s: %s" % (path, summary))
    if failed:
        sys.exit("check_elf: %d 个文件未通过校验（目标 %s）" % (failed, args.triple))
    print("check_elf: %d 个文件全部通过（目标 %s，形态 %s）" % (len(args.files), args.triple, kind))


if __name__ == "__main__":
    main()
