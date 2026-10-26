// Runs the demos built through aqr-ld (see e2e.sh) on host.mjs.
import { readFileSync } from "node:fs";
import assert from "node:assert/strict";
import { apply } from "./host.mjs";

const dir = process.argv[2];
const load = (name) => readFileSync(`${dir}/aqr_demo_${name}.wasm`);
const bytes = (s) => new TextEncoder().encode(s);
const seq = (n, f) => Uint8Array.from({ length: n }, (_, i) => f(i) & 255);
const xor = (data, key) => data.map((b, i) => b ^ key[i % key.length]);

const concat = load("concat"), xorMod = load("xor"), splitMod = load("split");
const split = (data) => [0, 1].map((j) => data.filter((_, i) => i % 2 === j));
const cases = {
    "concat": () => assert.deepEqual(
        apply(concat, { inputs: [bytes("in0"), bytes("input1")], resources: [bytes("res")], params: bytes("|"), sizes: [13] })[0],
        bytes("resin0|input1")),
    "concat large": () => {
        const in0 = seq(100000, (i) => i);
        assert.deepEqual(apply(concat, { inputs: [in0, bytes("")], resources: [bytes("")], params: bytes(""), sizes: [100000] })[0], in0);
    },
    "concat wrong size": () => {
        const logs = [];
        assert.throws(() =>
            apply(concat, { inputs: [bytes("in0"), bytes("input1")], resources: [bytes("res")], params: bytes("|"), sizes: [12], logs }),
            WebAssembly.RuntimeError);
        assert.match(logs.at(-1), /^panicked at aqr-api\/demo\/concat\/src\/lib\.rs:\d+:\d+$/);
    },
    "xor": () => assert.deepEqual(
        apply(xorMod, { inputs: [bytes("hello")], resources: [], params: bytes("k1"), sizes: [5] })[0], xor(bytes("hello"), bytes("k1"))),
    "xor empty key": () => {
        const logs = [];
        assert.throws(() => apply(xorMod, { inputs: [bytes("hello")], resources: [], params: bytes(""), sizes: [5], logs }),
            WebAssembly.RuntimeError);
        assert.deepEqual(logs, ["xor: key of 1 to 256 bytes expected"]);
    },
    "xor multi-page": () => {
        const data = seq(200003, (i) => i * 7), key = bytes("some key");
        assert.deepEqual(apply(xorMod, { inputs: [data], resources: [], params: key, sizes: [data.length] })[0], xor(data, key));
    },
    "split": () => {
        for (const data of [bytes("hello"), bytes(""), bytes("x"), seq(200003, (i) => i * 7)]) {
            const sizes = [Math.ceil(data.length / 2), Math.floor(data.length / 2)];
            assert.deepEqual(apply(splitMod, { inputs: [data], resources: [], params: bytes(""), sizes }), split(data));
        }
    },
    "split wrong count": () =>
        assert.throws(() => apply(splitMod, { inputs: [bytes("hello")], resources: [], params: bytes(""), sizes: [3] })),
};
let failed = 0;
for (const [name, f] of Object.entries(cases)) {
    try { f(); console.log(`ok   ${name}`); } catch (e) { failed++; console.log(`FAIL ${name}: ${e.message}`); }
}
process.exit(failed ? 1 : 0);
