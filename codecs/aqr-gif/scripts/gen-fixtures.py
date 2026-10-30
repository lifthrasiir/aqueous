"""Writes tests/data: small GIF files of several encoders used as black boxes (ImageMagick,
ffmpeg, Pillow and macOS's ImageIO through `sips`), of ImageMagick's built-in images, and a
file cut in its image data. Run with `python3 -I`, from anywhere, with the repository's .venv
(Pillow) as the interpreter."""

import os
import subprocess
import tempfile

from PIL import Image

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(HERE, '..', 'tests', 'data')


def run(*args):
    subprocess.run(args, check=True, stdout=subprocess.DEVNULL)


def out(name):
    return os.path.join(OUT, name)


with tempfile.TemporaryDirectory() as tmp:
    photo = os.path.join(tmp, 'photo.png')
    art = os.path.join(tmp, 'art.png')
    # a photograph (70 x 46) and line art with flat colors
    run('magick', 'rose:', photo)
    run('magick', 'logo:', '-resize', '160x120', art)

    for name, src in [('photo', photo), ('art', art)]:
        # ImageMagick: dithered to 256 colors, 16 colors, interlaced
        run('magick', src, out(f'im-{name}.gif'))
        run('magick', src, '-colors', '16', out(f'im16-{name}.gif'))
        run('magick', src, '-interlace', 'GIF', out(f'im-il-{name}.gif'))
        run('ffmpeg', '-loglevel', 'error', '-y', '-i', src, out(f'ff-{name}.gif'))
        run('sips', '-s', 'format', 'gif', src, '--out', out(f'sips-{name}.gif'))
        im = Image.open(src).convert('RGB')
        im.quantize(256).save(out(f'pil-{name}.gif'))
        im.quantize(4).save(out(f'pil4-{name}.gif'), interlace=True)

    # an animation of frames of other sizes and places (ImageMagick optimizes them)
    run('magick', '-delay', '10', photo, '(', photo, '-rotate', '90', ')', '(', photo, '-flop', ')',
        '-layers', 'optimize', out('im-anim.gif'))
    # ffmpeg's animation of a test pattern
    run('ffmpeg', '-loglevel', 'error', '-y', '-f', 'lavfi', '-i', 'testsrc=duration=0.5:size=64x48:rate=8',
        out('ff-anim.gif'))

# a file cut in its image data, as a download stopped
with open(out('im-photo.gif'), 'rb') as f:
    d = f.read()
with open(out('cut-photo.gif'), 'wb') as f:
    f.write(d[:len(d) * 2 // 3])
