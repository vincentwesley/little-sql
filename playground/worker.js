// Runs the engine off the main thread so a long query never freezes the
// page. Messages in: {id, op: "open", bytes} | {id, op: "exec", sql}.
// Messages out: {id, ok, ...}.
import { Engine } from "./engine.js";

let engine = null;
let lastBytes = null;

async function fresh() {
  const wasm = await (await fetch("littlesql.wasm")).arrayBuffer();
  engine = await Engine.load(wasm);
}

self.onmessage = async (ev) => {
  const { id, op } = ev.data;
  try {
    if (!engine) await fresh();
    if (op === "open") {
      lastBytes = ev.data.bytes;
      engine.open(lastBytes);
      self.postMessage({ id, ok: true });
    } else if (op === "export") {
      const bytes = engine.export();
      self.postMessage({ id, ok: !!bytes, bytes, error: bytes ? null : "nothing to export" });
    } else if (op === "exec") {
      const t0 = performance.now();
      const report = engine.exec(ev.data.sql, { maxRows: 1000, maxSteps: ev.data.maxSteps });
      self.postMessage({ id, ok: true, report, millis: performance.now() - t0 });
    }
  } catch (e) {
    // A trap (a bug in the engine) leaves the module unusable: start a new
    // one and reopen the database so the page keeps working.
    let note = String(e && e.message ? e.message : e);
    if (e instanceof WebAssembly.RuntimeError) {
      note = `The engine crashed (${note}). This is a bug in Little SQL; the database was reloaded.`;
      try {
        await fresh();
        if (lastBytes) engine.open(lastBytes);
      } catch (_) {
        engine = null;
      }
    }
    self.postMessage({ id, ok: false, error: note });
  }
};
