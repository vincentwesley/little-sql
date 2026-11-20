// A thin wrapper around littlesql.wasm's C-ABI exports. Used by the
// playground's web worker and by scripts/playground-smoke.mjs in Node.

const enc = new TextEncoder();
const dec = new TextDecoder();

export class Engine {
  static async load(wasmBytes) {
    const { instance } = await WebAssembly.instantiate(wasmBytes, {});
    return new Engine(instance.exports);
  }

  constructor(exports) {
    this.x = exports;
  }

  // Copy bytes into a fresh buffer inside the module; returns its pointer.
  put(bytes) {
    const p = this.x.ls_alloc(bytes.length);
    new Uint8Array(this.x.memory.buffer, p, bytes.length).set(bytes);
    return p;
  }

  out() {
    const p = this.x.ls_out_ptr();
    const n = this.x.ls_out_len();
    return new Uint8Array(this.x.memory.buffer, p, n).slice();
  }

  // Open a database file (Uint8Array), or an empty database.
  open(bytes) {
    const n = bytes ? bytes.length : 0;
    const ok = this.x.ls_open(n ? this.put(bytes) : 0, n);
    if (!ok) throw new Error(JSON.parse(dec.decode(this.out())).error);
  }

  // Run SQL; returns { results: [...], error }.
  exec(sql, { maxRows = 1000, maxSteps = 50_000_000 } = {}) {
    this.x.ls_set_now(Date.now());
    const b = enc.encode(sql);
    this.x.ls_exec(this.put(b), b.length, maxRows, maxSteps);
    return JSON.parse(dec.decode(this.out()));
  }

  // The current database file image.
  export() {
    return this.x.ls_export() ? this.out() : null;
  }
}
