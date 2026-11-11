"""Read the video chunks of an AVI (also a truncated one) sequentially."""
import struct
def video_frames(path, start=0, limit=None):
    f = open(path, 'rb'); d = f.read(1 << 20); mi = d.index(b'movi'); pos = mi + 4; n = 0
    while True:
        f.seek(pos); h = f.read(8)
        if len(h) < 8: break
        cid = h[:4]; sz = struct.unpack('<I', h[4:])[0]
        if cid == b'LIST': pos += 12; continue
        if not (cid[:2].isdigit() and cid[2:] in (b'dc', b'wb', b'db')) and cid != b'JUNK': break
        if cid == b'00dc':
            data = f.read(sz)
            if len(data) < sz: break
            if n >= start: yield n, data
            n += 1
            if limit and n >= start + limit: break
        pos += 8 + sz + (sz & 1)
