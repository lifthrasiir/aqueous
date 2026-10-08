"""Re-export real meshes with Blender's OBJ exporter, into corpus/writers/*-blender.obj.

usage: /Applications/Blender.app/Contents/MacOS/Blender --background --factory-startup \\
           --python scripts/gen-blender.py
"""
import os

import bpy

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SOURCES = ["corpus/c3d/igea.obj", "corpus/c3d/xyzrgb_dragon.obj", "corpus/mcguire/crytek_sponza.obj",
           "corpus/mcguire/conference.obj"]

for src in SOURCES:
    dest = os.path.join(ROOT, "corpus", "writers", os.path.splitext(os.path.basename(src))[0] + "-blender.obj")
    if os.path.exists(dest):
        continue
    bpy.ops.wm.read_factory_settings(use_empty=True)
    bpy.ops.wm.obj_import(filepath=os.path.join(ROOT, src))
    bpy.ops.wm.obj_export(filepath=dest + ".tmp.obj", export_materials=False)
    os.replace(dest + ".tmp.obj", dest)
    print(f"ok    {os.path.relpath(dest, ROOT)} ({os.path.getsize(dest)} bytes)")
