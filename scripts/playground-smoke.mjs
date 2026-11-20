#!/usr/bin/env node
// Smoke test for the built playground (playground/dist): load the wasm
// engine in Node through the same engine.js the browser uses, open the
// demo database, run a query, an index search and an EXPLAIN, and check
// the answers. Exits non-zero on any failure.

import fs from "node:fs";
import path from "node:path";
import assert from "node:assert/strict";

const dist = path.resolve(path.dirname(new URL(import.meta.url).pathname), "../playground/dist");
const { Engine } = await import(path.join(dist, "engine.js"));

const engine = await Engine.load(fs.readFileSync(path.join(dist, "littlesql.wasm")));
engine.open(new Uint8Array(fs.readFileSync(path.join(dist, "demo.db"))));

const one = (sql) => {
  const r = engine.exec(sql);
  assert.equal(r.error, null, `${sql}: ${r.error}`);
  return r.results.at(-1);
};

// 1. A plain query.
const towns = one("SELECT count(*) FROM towns");
assert.equal(towns.rows[0][0][1], "26");

// 2. An index search, with the plan and its row counts.
const r = one("SELECT count(*), max(price) FROM resale WHERE town_id = 1 AND month >= '2026-01'");
const search = r.plan.find((l) => l.text.includes("SEARCH resale USING"));
assert.ok(search, `expected an index search, got ${JSON.stringify(r.plan)}`);
assert.ok(search.rows > 0 && search.rows < 10000, `index search visited ${search.rows} rows`);

// 3. EXPLAIN QUERY PLAN.
const eqp = one("EXPLAIN QUERY PLAN SELECT * FROM sales WHERE town = 'BEDOK'");
assert.ok(eqp.explain);
assert.ok(eqp.plan.length >= 2, JSON.stringify(eqp.plan));

// 4. Errors come back as text, and the engine keeps working.
const bad = engine.exec("SELECT * FROM no_such_table");
assert.match(bad.error, /no such table/);
assert.equal(one("SELECT 1 + 1").rows[0][0][1], "2");

// 5. The test report is there and well formed.
const results = JSON.parse(fs.readFileSync(path.join(dist, "results.json"), "utf8"));
assert.ok(results.suites.length >= 2);

// 6. Every example on the page runs without an error.
const app = fs.readFileSync(path.join(dist, "app.js"), "utf8");
const examples = [...app.matchAll(/sql: `([^`]*)`/g)].map((m) => m[1]);
assert.ok(examples.length >= 5);
for (const sql of examples) one(sql);

console.log(`playground smoke test passed (${examples.length} examples ran)`);
