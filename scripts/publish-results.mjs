#!/usr/bin/env node
// Publish test results: copy the runner's reports from results/ into
// docs/results/ (committed), combine them into docs/results/results.json
// (what the playground shows) and rewrite the table in README.md.
//
//   node scripts/publish-results.mjs           # publish
//   node scripts/publish-results.mjs --check   # fail if anything is stale
//
// --check verifies that every fresh report in results/ equals the
// committed copy, that README's table matches the committed reports, and
// that playground/dist/results.json (if built) matches too.

import fs from "node:fs";
import path from "node:path";

const root = path.resolve(path.dirname(new URL(import.meta.url).pathname), "..");
const R = (...p) => path.join(root, ...p);
const check = process.argv.includes("--check");

// In display order. `label` is the README row.
const REPORTS = [
  { file: "slt-read-full.json", label: (s) => `sqllogictest, full corpus (${s.files} files), read mode` },
  { file: "slt-native-full.json", label: (s) => `sqllogictest, full corpus (${s.files} files), native mode` },
  { file: "slt-read.json", label: (s) => `sqllogictest, CI sample (${s.files} files), read mode` },
  { file: "slt-native.json", label: (s) => `sqllogictest, CI sample (${s.files} files), native mode` },
  { file: "differential.json", label: (s) => `Differential tests against ${s.reference}` },
];

const read = (p) => (fs.existsSync(p) ? fs.readFileSync(p, "utf8") : null);
const num = (n) => Number(n).toLocaleString("en");
let problems = [];

// 1. Fresh reports -> committed copies.
fs.mkdirSync(R("docs/results"), { recursive: true });
for (const { file } of REPORTS) {
  const fresh = read(R("results", file));
  if (fresh === null) continue;
  const committed = read(R("docs/results", file));
  if (check) {
    if (fresh !== committed) problems.push(`docs/results/${file} is not the latest result (run publish-results.mjs)`);
  } else {
    fs.writeFileSync(R("docs/results", file), fresh);
  }
}

// 2. Combined report and README table from the committed copies.
const suites = [];
const rows = [];
for (const r of REPORTS) {
  const text = read(R("docs/results", r.file));
  if (text === null) continue;
  const s = JSON.parse(text);
  suites.push(s);
  rows.push(`| ${r.label(s)} | ${num(s.total.pass)} | ${num(s.total.fail)} | ${s.total.pass_rate.toFixed(2)} % |`);
}
const table = [
  "| Suite | Passed | Failed | Pass rate |",
  "|-------|-------:|-------:|----------:|",
  ...rows,
].join("\n");

const combinedPath = R("docs/results/results.json");
const old = read(combinedPath);
const date = check && old ? JSON.parse(old).date : new Date().toISOString().slice(0, 10);
const combined = JSON.stringify(
  { reference: suites[0]?.reference ?? "sqlite3", engine: suites[0]?.engine ?? "littlesql", date, suites },
  null,
  1,
) + "\n";

const readme = read(R("README.md"));
const start = "<!-- results:start -->\n";
const end = "\n<!-- results:end -->";
const a = readme.indexOf(start);
const b = readme.indexOf(end);
if (a < 0 || b < 0) throw new Error("README.md has no results markers");
const newReadme = readme.slice(0, a + start.length) + table + readme.slice(b);

if (check) {
  if (old !== combined) problems.push("docs/results/results.json is stale");
  if (readme !== newReadme) problems.push("README.md's results table is stale");
  const dist = read(R("playground/dist/results.json"));
  if (dist !== null && dist !== old) problems.push("playground/dist/results.json is stale (rebuild the playground)");
  if (problems.length) {
    for (const p of problems) console.error(p);
    process.exit(1);
  }
  console.log("published results are current");
} else {
  fs.writeFileSync(combinedPath, combined);
  fs.writeFileSync(R("README.md"), newReadme);
  console.log(`published ${suites.length} reports:\n${table}`);
}
