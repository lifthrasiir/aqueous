// Rewrite real meshes as three.js's OBJExporter prints them (Float32Array values through
// Number's toString), into corpus/writers/*-js-f32.obj. usage: node scripts/gen-writers.mjs
import { readFileSync, writeFileSync, existsSync, renameSync } from "node:fs";
import { dirname, join, basename } from "node:path";
import { fileURLToPath } from "node:url";

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const sources = ["corpus/c3d/igea.obj", "corpus/c3d/xyzrgb_dragon.obj", "corpus/mcguire/crytek_sponza.obj"];
const f = new Float32Array(1);
for (const src of sources) {
  const dest = join(root, "corpus/writers", basename(src, ".obj") + "-js-f32.obj");
  if (existsSync(dest)) continue;
  const out = readFileSync(join(root, src), "latin1").split("\n").map((l) => {
    const t = l.trim().split(/\s+/);
    if (["v", "vt", "vn"].includes(t[0]) && t.length > 1) {
      return t[0] + " " + t.slice(1).map((x) => { f[0] = parseFloat(x); return String(f[0]); }).join(" ");
    }
    return l.replace(/\r$/, "");
  });
  writeFileSync(dest + ".tmp", out.join("\n"), "latin1");
  renameSync(dest + ".tmp", dest);
  console.error(`ok    ${dest}`);
}
