"""Writes the test fixtures (synthetic data) with h5py: `.venv/Scripts/python -I make.py` here.

h5py 3.16 (HDF5 1.14), zlib's deflate. Each file is small; what each covers is in its name.
"""
import os, h5py, numpy as np

here = os.path.dirname(os.path.abspath(__file__))
rng = np.random.default_rng(1)
# a smooth signal with noise: what a chunk's data is like
wave = (1000 * np.sin(np.arange(12000) / 50) + rng.normal(0, 20, 12000)).astype('<i2')
temp = np.cumsum(rng.normal(0, 0.01, 3000)).astype('<f8')
img = (np.add.outer(np.arange(64), np.arange(96)) * 3 + rng.integers(0, 8, (64, 96))).astype('<u2')

def path(n): return os.path.join(here, n)

# superblock 0, v1 object headers and B-trees, symbol tables
with h5py.File(path('sb0.h5'), 'w', libver='earliest') as f:
    f.create_dataset('wave', data=wave, chunks=(4096,), compression='gzip', compression_opts=6)
    f.create_dataset('temp', data=temp, chunks=(500,), compression="gzip", compression_opts=1,
                     shuffle=True, fletcher32=True)
    g = f.create_group('images')
    g.create_dataset('img', data=img, chunks=(32, 48), compression='gzip', compression_opts=9, shuffle=True)
    g.create_dataset('plain', data=img[:8], chunks=(8, 32))
    f.attrs['note'] = 'synthetic'

# superblock 3 (libver latest): v2 object headers, dense groups (more than 8 links), fixed
# array and single chunk indexes, an extensible array (unlimited, not read)
with h5py.File(path('sb3-dense.h5'), 'w', libver='latest') as f:
    g = f.create_group('many')
    for i in range(12):
        g.create_dataset(f'd{i:02}', data=wave[i * 1000:(i + 1) * 1000], chunks=(250,), compression='gzip')
    f.create_dataset('single', data=temp, chunks=temp.shape, compression='gzip', shuffle=True)
    f.create_dataset('grow', data=wave[:5000], chunks=(1000,), maxshape=(None,), compression='gzip')
    f.create_dataset('img', data=img, chunks=(32, 64), compression='gzip', fletcher32=True)

# a user block before the superblock, and chunks whose mask skips deflate (written as they are)
with h5py.File(path('userblock.h5'), 'w', libver='earliest', userblock_size=512) as f:
    d = f.create_dataset('wave', shape=(12000,), dtype=wave.dtype, chunks=(3000,), compression='gzip')
    d[:6000] = wave[:6000]
    for k in (6000, 9000):
        d.id.write_direct_chunk((k,), wave[k:k + 3000].tobytes(), filter_mask=1)
with open(path('userblock.h5'), 'r+b') as u:
    u.write(b'user block of a test')
