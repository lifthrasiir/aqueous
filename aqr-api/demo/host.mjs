// A host for the spec's "apply a module" on Node, for testing aqr-ld outputs.
// Needs --experimental-wasm-type-reflection for the minimum of the working memory.

const PAGE = 65536;

function sizedToFit(data, len = data.length) {
    const pages = BigInt(Math.ceil(len / PAGE));
    const mem = new WebAssembly.Memory({ initial: pages, maximum: pages, address: "i64" });
    if (data) new Uint8Array(mem.buffer).set(data);
    return mem;
}

/// Returns the output, or throws for an invalid archive failure. `aqr_log` messages go to `logs`.
export function apply(bytes, { inputs, resources, params, size, logs = [] }) {
    const mod = new WebAssembly.Module(bytes);
    const whole = WebAssembly.Module.exports(mod).some((e) => e.name === "aqr_apply_whole");
    const entry = whole ? "aqr_apply_whole" : "aqr_apply_part";
    const visible = inputs.map(() => [0, 0]);
    const committed = [];
    const mems = {};
    const host = {
        aqr_request(k, hi) {
            hi = Number(hi);
            const w = mems[`aqr_input${k}`].buffer.byteLength, [lo, h] = visible[k];
            if (!(h <= hi && hi <= inputs[k].length && hi - lo <= w)) throw new Error(`aqr_request(${k}, ${hi})`);
            const mem = new Uint8Array(mems[`aqr_input${k}`].buffer);
            for (let o = h; o < hi; o++) mem[o % w] = inputs[k][o];
            visible[k][1] = hi;
        },
        aqr_release(k, lo) {
            lo = Number(lo);
            if (!(visible[k][0] <= lo && lo <= visible[k][1])) throw new Error(`aqr_release(${k}, ${lo})`);
            visible[k][0] = lo;
        },
        aqr_commit(hi) {
            hi = Number(hi);
            const mem = new Uint8Array(mems.aqr_output.buffer), w = mem.length;
            if (!(committed.length <= hi && hi <= size && hi - committed.length <= w)) throw new Error(`aqr_commit(${hi})`);
            for (let o = committed.length; o < hi; o++) committed.push(mem[o % w]);
        },
        aqr_heartbeat() {},
        aqr_log(ptr, len) {
            // A nop for the application: an out-of-bounds range is ignored, a long message truncated.
            const mem = mems.aqr_memory.buffer;
            if (ptr >>> 0 <= mem.byteLength && len >>> 0 <= mem.byteLength - (ptr >>> 0)) {
                logs.push(new TextDecoder().decode(new Uint8Array(mem, ptr >>> 0, Math.min(len >>> 0, 1024))));
            }
        },
    };
    const aqr = {};
    for (const imp of WebAssembly.Module.imports(mod)) {
        if (imp.module !== "aqr") throw new Error(`import ${imp.module}.${imp.name}`);
        const n = imp.name;
        let m;
        if (imp.kind === "function") {
            if (!(n in host) || (whole && !(n === "aqr_heartbeat" || n === "aqr_log"))) throw new Error(`import ${n}`);
            aqr[n] = host[n];
            continue;
        } else if (n === "aqr_memory") m = new WebAssembly.Memory({ initial: imp.type.minimum });
        else if (n === "aqr_params") m = sizedToFit(params);
        else if (n === "aqr_output") m = sizedToFit(null, size);
        else if (n.startsWith("aqr_resource")) m = sizedToFit(resources[+n.slice(12)] ?? fail(n));
        else if (n.startsWith("aqr_input")) {
            const data = inputs[+n.slice(9)] ?? fail(n);
            m = whole ? sizedToFit(data) : sizedToFit(null, data.length);
        } else throw new Error(`import ${n}`);
        aqr[n] = mems[n] = m;
    }
    const instance = new WebAssembly.Instance(mod, { aqr });
    const args = [params.length, ...resources.map((r) => r.length), ...inputs.map((i) => i.length), size];
    instance.exports[entry](...args.map(BigInt));
    const output = whole ? (mems.aqr_output ? new Uint8Array(mems.aqr_output.buffer, 0, size) : new Uint8Array(size)) : Uint8Array.from(committed);
    if (output.length !== size) throw new Error(`output of ${output.length} bytes, not ${size}`);
    return output;
}

function fail(what) {
    throw new Error(`${what} beyond the data`);
}
