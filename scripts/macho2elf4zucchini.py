#!/usr/bin/env python3
"""A thin 64-bit Mach-O image wrapped as an ELF64 file for Zucchini (Chromium's executable
differ, which knows PE and ELF but takes Mach-O as plain data), so that it disassembles the
code: the Mach-O bytes kept as they are at BASE, a PT_LOAD for each segment and a section
header for each section, code sections marked SHF_EXECINSTR. Not meant to load or run.

What Zucchini needs beyond a plain wrapping, found by trying (its code was not read):
addresses below 4 GiB (all moved down by the first segment's, which leaves PC-relative
references as they are), and no two SHT_NOBITS sections at one offset (zero-fill sections are
left out). Chained fixups are not turned into relocations, so only PC-relative references are
seen. Used for docs/macos-app-todo.md's appendix A.

usage: python3 scripts/macho2elf4zucchini.py IN OUT"""
import struct
import sys

BASE = 0x4000
MH_MAGIC_64 = 0xFEEDFACF
LC_SEGMENT_64 = 0x19
CPU = {0x0100000C: 183, 0x01000007: 62}  # ARM64 -> EM_AARCH64, X86_64 -> EM_X86_64
S_ZEROFILL = (0x1, 0xC, 0x12)
S_ATTR_PURE_INSTRUCTIONS = 0x80000000
S_ATTR_SOME_INSTRUCTIONS = 0x00000400


def main(src, dst):
    d = open(src, 'rb').read()
    magic, cputype, _, _, ncmds, _, _, _ = struct.unpack_from('<IIIIIIII', d)
    assert magic == MH_MAGIC_64, 'a thin 64-bit Mach-O file'
    segs, sects = [], []
    o = 32
    for _ in range(ncmds):
        cmd, size = struct.unpack_from('<II', d, o)
        if cmd == LC_SEGMENT_64:
            segname, vmaddr, vmsize, fileoff, filesize, maxprot, initprot, nsects, _ = struct.unpack_from(
                '<16sQQQQiiII', d, o + 8)
            segname = segname.rstrip(b'\0')
            if segname != b'__PAGEZERO':
                segs.append((vmaddr, vmsize, fileoff, filesize, initprot))
            s = o + 72
            for _ in range(nsects):
                sectname, sname, addr, ssize, off, align, _, _, flags = struct.unpack_from('<16s16sQQIIIII', d, s)
                sects.append((sname.rstrip(b'\0') + b',' + sectname.rstrip(b'\0'), addr, ssize, off, align, flags,
                              initprot))
                s += 80
        o += size
    # Addresses above 4 GiB are not taken: everything moved down by the first segment's (all the
    # references of interest are PC-relative, so they do not change)
    vbase = segs[0][0]
    segs = [(g[0] - vbase,) + g[1:] for g in segs]
    sects = [c[:1] + (c[1] - vbase,) + c[2:] for c in sects]
    # Section names
    names = bytearray(b'\0')
    shstr = len(names)
    names += b'.shstrtab\0'
    shdrs = [bytes(64)]
    tail = BASE + len(d)
    # Zero-fill sections hold no bytes; several at one offset are rejected
    sects = [c for c in sects if (c[5] & 0xFF) not in S_ZEROFILL]
    for name, addr, size, off, align, flags, prot in sects:
        n = len(names)
        names += name + b'\0'
        zerofill = (flags & 0xFF) in S_ZEROFILL
        f = 2  # SHF_ALLOC
        if prot & 2:
            f |= 1  # SHF_WRITE
        if flags & (S_ATTR_PURE_INSTRUCTIONS | S_ATTR_SOME_INSTRUCTIONS):
            f |= 4  # SHF_EXECINSTR
        shdrs.append(struct.pack('<IIQQQQIIQQ', n, 8 if zerofill else 1, f, addr, tail if zerofill else BASE + off,
                                 size, 0, 0, 1 << align, 0))
    shnames_off = tail
    shdrs.append(struct.pack('<IIQQQQIIQQ', shstr, 3, 0, 0, shnames_off, len(names), 0, 0, 1, 0))
    shoff = (shnames_off + len(names) + 7) & ~7
    phdrs = []
    for vmaddr, vmsize, fileoff, filesize, prot in segs:
        flags = (4 if prot & 1 else 0) | (2 if prot & 2 else 0) | (1 if prot & 4 else 0)
        phdrs.append(struct.pack('<IIQQQQQQ', 1, flags, BASE + fileoff, vmaddr, vmaddr, filesize, vmsize, 0x4000))
    ehdr = struct.pack('<4sBBBBB7sHHIQQQIHHHHHH', b'\x7fELF', 2, 1, 1, 0, 0, bytes(7), 3, CPU[cputype], 1,
                       0, 64, shoff, 0, 64, 56, len(phdrs), 64, len(shdrs), len(shdrs) - 1)
    head = ehdr + b''.join(phdrs)
    assert len(head) <= BASE
    out = bytearray(head + bytes(BASE - len(head)) + d + names)
    out += bytes(shoff - len(out)) + b''.join(shdrs)
    open(dst, 'wb').write(out)


if __name__ == '__main__':
    main(*sys.argv[1:])
