"""The AC-3 byte stream of an AVI (the `01wb` chunks in order) and its syncframes."""
import struct, json, os
HERE = os.path.dirname(os.path.abspath(__file__))
T = json.load(open(os.path.join(HERE, 'ac3_tables.json')))
def audio_chunks(path, ids=(b'01wb',), limit=None):
    f = open(path, 'rb'); d = f.read(1 << 20); mi = d.index(b'movi'); pos = mi + 4; out = []
    while True:
        f.seek(pos); h = f.read(8)
        if len(h) < 8: break
        cid = h[:4]; sz = struct.unpack('<I', h[4:])[0]
        if cid == b'LIST': pos += 12; continue
        if not (cid[:2].isdigit() and cid[2:] in (b'dc', b'wb', b'db')) and cid != b'JUNK': break
        if cid in ids:
            data = f.read(sz)
            if len(data) < sz: break
            out.append(data)
            if limit and len(out) >= limit: break
        pos += 8 + sz + (sz & 1)
    return out
def syncframes(buf):
    """(offset, size in bytes, fscod, frmsizecod) of the syncframes in order, from the start."""
    i = 0; out = []
    while i + 5 <= len(buf):
        if buf[i] == 0x0B and buf[i + 1] == 0x77:
            fscod = buf[i + 4] >> 6; frm = buf[i + 4] & 0x3f
            if fscod < 3 and frm < 38:
                size = T['frame_words'][frm][fscod] * 2
                out.append((i, size, fscod, frm)); i += size; continue
        i += 1
    return out
if __name__ == '__main__':
    import sys
    ch = audio_chunks('F:/Data/aqr-corpus/aqr-div3/mari.avi', limit=3000)
    buf = b''.join(ch); fr = syncframes(buf)
    print(len(ch), 'chunks', len(buf), 'bytes', len(fr), 'frames; first', fr[:3], 'gaps', sum(1 for a, b in zip(fr, fr[1:]) if a[0] + a[1] != b[0]))
