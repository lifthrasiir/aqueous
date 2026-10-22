// Runs the demos built through aqr-ld (see e2e.sh) on host.mjs.
import { readFileSync } from "node:fs";
import assert from "node:assert/strict";
import { apply } from "./host.mjs";

const dir = process.argv[2];
const load = (name) => readFileSync(`${dir}/aqr_demo_${name}.wasm`);
const bytes = (s) => new TextEncoder().encode(s);
const seq = (n, f) => Uint8Array.from({ length: n }, (_, i) => f(i) & 255);
const xor = (data, key) => data.map((b, i) => b ^ key[i % key.length]);

const concat = load("concat"), xorMod = load("xor");
const cases = {
    "concat": () => assert.deepEqual(
        apply(concat, { inputs: [bytes("in0"), bytes("input1")], resources: [bytes("res")], params: bytes("|"), size: 13 }),
        bytes("resin0|input1")),
    "concat large": () => {
        const in0 = seq(100000, (i) => i);
        assert.deepEqual(apply(concat, { inputs: [in0, bytes("")], resources: [bytes("")], params: bytes(""), size: 100000 }), in0);
    },
    "concat wrong size": () => {
        const logs = [];
        assert.throws(() =>
            apply(concat, { inputs: [bytes("in0"), bytes("input1")], resources: [bytes("res")], params: bytes("|"), size: 12, logs }),
            WebAssembly.RuntimeError);
        assert.match(logs.at(-1), /^panicked at aqr-api\/demo\/concat\/src\/lib\.rs:\d+:\d+$/);
    },
    "xor": () => assert.deepEqual(
        apply(xorMod, { inputs: [bytes("hello")], resources: [], params: bytes("k1"), size: 5 }), xor(bytes("hello"), bytes("k1"))),
    "xor empty key": () => {
        const logs = [];
        assert.throws(() => apply(xorMod, { inputs: [bytes("hello")], resources: [], params: bytes(""), size: 5, logs }),
            WebAssembly.RuntimeError);
        assert.deepEqual(logs, ["xor: key of 1 to 256 bytes expected"]);
    },
    "xor multi-page": () => {
        const data = seq(200003, (i) => i * 7), key = bytes("some key");
        assert.deepEqual(apply(xorMod, { inputs: [data], resources: [], params: key, size: data.length }), xor(data, key));
    },
};
let failed = 0;
for (const [name, f] of Object.entries(cases)) {
    try { f(); console.log(`ok   ${name}`); } catch (e) { failed++; console.log(`FAIL ${name}: ${e.message}`); }
}
process.exit(failed ? 1 : 0);
