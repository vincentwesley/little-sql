// The playground page: talks to the engine in worker.js, draws results,
// query plans and the test report.

const $ = (s) => document.querySelector(s);

const EXAMPLES = [
  {
    title: "Average price by town, 2026",
    sql: `SELECT town, count(*) AS sales, round(avg(price)) AS avg_price
FROM sales
WHERE month >= '2026-01'
GROUP BY town
ORDER BY avg_price DESC;`,
  },
  {
    title: "Ten most expensive flats",
    sql: `SELECT month, town, flat_type, block, street, storey_range, price
FROM sales
ORDER BY price DESC
LIMIT 10;`,
  },
  {
    title: "Index search: one town, one year",
    sql: `-- The plan shows an index search on (town_id, month), not a scan.
SELECT month, flat_type, storey_range, price
FROM resale
WHERE town_id = (SELECT id FROM towns WHERE name = 'PUNGGOL')
  AND month BETWEEN '2026-01' AND '2026-12'
ORDER BY month DESC, price DESC
LIMIT 20;`,
  },
  {
    title: "Price per square metre by region and year",
    sql: `SELECT t.region, substr(r.month, 1, 4) AS year,
       round(avg(r.price / r.floor_area_sqm)) AS price_per_sqm
FROM resale r JOIN towns t ON t.id = r.town_id
WHERE r.flat_type = '4 ROOM'
GROUP BY 1, 2
ORDER BY 1, 2;`,
  },
  {
    title: "Million-dollar flats per year",
    sql: `SELECT substr(month, 1, 4) AS year,
       count(*) FILTER (WHERE price >= 1000000) AS million_plus,
       count(*) AS all_sales,
       printf('%.2f%%', 100.0 * count(*) FILTER (WHERE price >= 1000000) / count(*)) AS share
FROM resale
GROUP BY year
ORDER BY year;`,
  },
  {
    title: "Covering index: price range by flat type",
    sql: `-- Answered from the index alone (COVERING INDEX in the plan).
SELECT flat_type, min(price), max(price), count(*)
FROM resale
WHERE flat_type IN ('3 ROOM', '4 ROOM', '5 ROOM')
GROUP BY flat_type;`,
  },
  {
    title: "Subquery: towns above the national average",
    sql: `SELECT name, region,
       (SELECT round(avg(price)) FROM resale r WHERE r.town_id = t.id) AS avg_price
FROM towns t
WHERE (SELECT avg(price) FROM resale r WHERE r.town_id = t.id)
      > (SELECT avg(price) FROM resale)
ORDER BY avg_price DESC;`,
  },
  {
    title: "Window functions: priciest flat per town, and running totals",
    sql: `SELECT town, month, flat_type, price,
       rank() OVER (PARTITION BY town ORDER BY price DESC) AS rank_in_town,
       sum(price) OVER (PARTITION BY town ORDER BY month
                        ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) AS running_total
FROM sales
WHERE month >= '2026-10'
ORDER BY town, rank_in_town
LIMIT 30;`,
  },
  {
    title: "JSON: each region as a document",
    sql: `SELECT region,
       json_group_array(json_object('town', name, 'sales', n)) AS towns
FROM (SELECT t.region, t.name, count(*) AS n
      FROM resale r JOIN towns t ON t.id = r.town_id
      WHERE r.month >= '2026-01'
      GROUP BY t.region, t.name)
GROUP BY region;`,
  },
  {
    title: "json_each: a JSON list as a table",
    sql: `SELECT j.value AS flat_type,
       count(s.price) AS sales,
       round(avg(s.price)) AS avg_price
FROM json_each('["3 ROOM", "4 ROOM", "5 ROOM", "EXECUTIVE"]') AS j
LEFT JOIN sales s ON s.flat_type = j.value AND s.month >= '2026-01'
GROUP BY j.key
ORDER BY j.key;`,
  },
  {
    title: "Recursive CTE: monthly sales with gaps filled",
    sql: `WITH RECURSIVE months(m) AS (
  SELECT '2025-01'
  UNION ALL
  SELECT strftime('%Y-%m', m || '-01', '+1 month') FROM months WHERE m < '2025-12'
)
SELECT m AS month,
       (SELECT count(*) FROM resale WHERE month = m AND town_id = 1) AS ang_mo_kio
FROM months;`,
  },
  {
    title: "Lease decay: remaining lease vs price",
    sql: `SELECT (2026 - lease_commence_year) / 10 * 10 AS age_band,
       count(*) AS sales,
       round(avg(price)) AS avg_price
FROM resale
WHERE flat_type = '4 ROOM' AND month >= '2026-01'
GROUP BY age_band
ORDER BY age_band;`,
  },
  {
    title: "Writing: a table of your own (then Download)",
    sql: `-- Changes stay in your browser; "Download this database" saves them.
CREATE TABLE IF NOT EXISTS shortlist(
  id INTEGER PRIMARY KEY,
  town TEXT NOT NULL,
  budget INT CHECK (budget > 0)
);
INSERT INTO shortlist(town, budget)
SELECT town, round(avg(price)) FROM sales
WHERE month >= '2026-01' AND flat_type = '4 ROOM'
GROUP BY town ORDER BY 2 LIMIT 5;
SELECT * FROM shortlist;`,
  },
  {
    title: "The schema itself",
    sql: `SELECT type, name, tbl_name, sql FROM sqlite_schema;`,
  },
];

// ------------------------------------------------------------ worker

const worker = new Worker("worker.js", { type: "module" });
let nextId = 1;
const pending = new Map();
worker.onmessage = (ev) => {
  const p = pending.get(ev.data.id);
  pending.delete(ev.data.id);
  if (p) p(ev.data);
};
worker.onerror = (ev) => {
  showError(`The engine could not start: ${ev.message || "unknown error"}. This page needs a browser with WebAssembly and module workers.`);
};
function call(msg) {
  return new Promise((resolve) => {
    const id = nextId++;
    pending.set(id, resolve);
    worker.postMessage({ ...msg, id });
  });
}

// ------------------------------------------------------------ helpers

function el(tag, attrs = {}, ...children) {
  const e = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (k === "class") e.className = v;
    else if (k === "text") e.textContent = v;
    else e.setAttribute(k, v);
  }
  for (const c of children) if (c != null) e.append(c);
  return e;
}

function fmtInt(n) {
  return Number(n).toLocaleString("en");
}

function showError(msg) {
  const e = $("#error");
  e.textContent = msg;
  e.hidden = !msg;
}

// Draw plan lines as the same tree sqlite3 prints.
function planText(lines) {
  const out = [];
  lines.forEach((line, i) => {
    let prefix = "";
    for (let d = 0; d < line.depth; d++) {
      let more = false;
      for (const l of lines.slice(i + 1)) {
        if (l.depth < d) break;
        if (l.depth === d) { more = true; break; }
      }
      prefix += more ? "|  " : "   ";
    }
    let last = true;
    for (const l of lines.slice(i + 1)) {
      if (l.depth < line.depth) break;
      if (l.depth === line.depth) { last = false; break; }
    }
    out.push({ text: prefix + (last ? "`--" : "|--") + line.text, line });
  });
  return out;
}

function renderPlan(lines) {
  const pre = el("pre", { class: "plan" });
  pre.append("QUERY PLAN\n");
  for (const { text, line } of planText(lines)) {
    pre.append(text);
    if (line.loops !== undefined) {
      pre.append(el("span", { class: "stat", text: `   loops=${fmtInt(line.loops)} rows=${fmtInt(line.rows)}` }));
    }
    pre.append("\n");
  }
  return pre;
}

function renderTable(r) {
  const table = el("table", { class: "grid" });
  const head = el("tr");
  for (const c of r.columns) head.append(el("th", { text: c }));
  table.append(el("thead", {}, head));
  const body = el("tbody");
  for (const row of r.rows) {
    const tr = el("tr");
    for (const [t, text] of row) tr.append(el("td", { class: t, text, title: text.length > 40 ? text : "" }));
    body.append(tr);
  }
  table.append(body);
  return el("div", { class: "table-wrap" }, table);
}

function renderResult(r) {
  const box = el("div", { class: "result" });
  const shown = r.rows.length < r.rowCount ? ` (showing the first ${fmtInt(r.rows.length)})` : "";
  const what = r.columns.length
    ? `${fmtInt(r.rowCount)} row${r.rowCount === 1 ? "" : "s"}${shown}`
    : r.changes ? `${fmtInt(r.changes)} row(s) changed` : "Done";
  // The database is in memory, so page reads are only counted when the
  // page cache had to load a page; zero says nothing, so it is not shown.
  const reads = r.pageReads
    ? [el("span", { class: "muted", text: `${fmtInt(r.pageReads)} page${r.pageReads === 1 ? "" : "s"} read` })]
    : [];
  box.append(el("div", { class: "result-head" },
    el("strong", { text: what }),
    ...reads,
    el("code", { text: r.sql.replace(/\s+/g, " ") })));
  if (r.plan.length) box.append(renderPlan(r.plan));
  if (r.columns.length && !r.explain) box.append(renderTable(r));
  return box;
}

// ------------------------------------------------------------ running

let busy = false;

async function run() {
  if (busy) return;
  const sql = $("#sql").value;
  if (!sql.trim()) return;
  busy = true;
  $("#run").disabled = true;
  $("#status").textContent = "Running…";
  showError("");
  const res = await call({ op: "exec", sql, maxSteps: 50_000_000 });
  busy = false;
  $("#run").disabled = false;
  const out = $("#results");
  out.replaceChildren();
  if (!res.ok) {
    $("#status").textContent = "";
    showError(res.error);
    return;
  }
  const { report, millis } = res;
  for (const r of report.results) out.append(renderResult(r));
  const n = report.results.length;
  $("#status").textContent = `${n} statement${n === 1 ? "" : "s"} in ${millis < 10 ? millis.toFixed(1) : Math.round(millis)} ms`;
  if (report.error) showError(report.error);
  if (report.results.some((r) => r.changes)) refreshSchema();
}

// ------------------------------------------------------------ databases

async function exec(sql) {
  const res = await call({ op: "exec", sql, maxSteps: 0 });
  if (!res.ok) throw new Error(res.error);
  if (res.report.error) throw new Error(res.report.error);
  return res.report.results;
}

async function refreshSchema() {
  const box = $("#schema");
  box.replaceChildren();
  let objects;
  try {
    [objects] = await exec("SELECT type, name, tbl_name FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%' ORDER BY type = 'index', type DESC, name");
  } catch (e) {
    box.append(el("p", { class: "muted", text: String(e.message) }));
    return;
  }
  const rows = objects.rows.map((r) => r.map((c) => c[1]));
  if (!rows.length) box.append(el("p", { class: "muted", text: "No tables yet." }));
  for (const [type, name] of rows.filter((r) => r[0] !== "index")) {
    const det = el("details", {}, el("summary", {}, name, el("span", { class: "kind", text: type })));
    const ul = el("ul");
    try {
      const [info] = await exec(`PRAGMA table_info("${name.replaceAll('"', '""')}")`);
      for (const c of info.rows) {
        const col = c[1][1], ty = c[2][1], pk = c[5][1] !== "0";
        ul.append(el("li", {}, col, " ", el("span", { text: `${ty}${pk ? " PK" : ""}` })));
      }
    } catch (_) { /* views over missing tables etc. */ }
    for (const [, ix] of rows.filter((r) => r[0] === "index" && r[2] === name)) {
      ul.append(el("li", { class: "ix", text: `index ${ix}` }));
    }
    det.append(ul);
    box.append(det);
  }
}

async function openBytes(bytes, label) {
  $("#db-status").textContent = "Opening…";
  const res = await call({ op: "open", bytes });
  if (!res.ok) {
    $("#db-status").textContent = "Could not open it.";
    showError(`${label}: ${res.error}`);
    return false;
  }
  showError("");
  $("#db-status").textContent = bytes ? `${label} — ${(bytes.length / 1048576).toFixed(1)} MB` : label;
  await refreshSchema();
  return true;
}

async function openDemo() {
  $("#db-status").textContent = "Downloading the demo database…";
  try {
    const resp = await fetch("demo.db");
    if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
    const bytes = new Uint8Array(await resp.arrayBuffer());
    await openBytes(bytes, "HDB resale prices");
  } catch (e) {
    $("#db-status").textContent = "The demo database could not be loaded.";
    showError(String(e.message || e));
  }
}

$("#db-select").addEventListener("change", async (ev) => {
  const v = ev.target.value;
  if (v === "demo") openDemo();
  else if (v === "empty") openBytes(null, "Empty in-memory database");
  else $("#file-input").click();
});

$("#file-input").addEventListener("change", async (ev) => {
  const f = ev.target.files[0];
  if (!f) return;
  const bytes = new Uint8Array(await f.arrayBuffer());
  await openBytes(bytes, f.name);
  ev.target.value = "";
});

$("#download").addEventListener("click", async () => {
  const res = await call({ op: "export" });
  if (!res.ok) return showError(res.error);
  const url = URL.createObjectURL(new Blob([res.bytes], { type: "application/vnd.sqlite3" }));
  const a = el("a", { href: url, download: "littlesql.db" });
  document.body.append(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
});

// ------------------------------------------------------------ editor

for (const [i, ex] of EXAMPLES.entries()) {
  $("#examples").append(el("option", { value: String(i), text: ex.title }));
}
$("#examples").addEventListener("change", (ev) => {
  $("#sql").value = EXAMPLES[Number(ev.target.value)].sql;
  run();
});
$("#run").addEventListener("click", run);
$("#sql").addEventListener("keydown", (ev) => {
  if ((ev.ctrlKey || ev.metaKey) && ev.key === "Enter") {
    ev.preventDefault();
    run();
  }
});

// ------------------------------------------------------------ pages

for (const tab of document.querySelectorAll(".tab")) {
  tab.addEventListener("click", () => {
    for (const t of document.querySelectorAll(".tab")) t.classList.toggle("active", t === tab);
    for (const p of document.querySelectorAll(".page")) p.classList.toggle("active", p.id === `page-${tab.dataset.page}`);
  });
}

// ------------------------------------------------------------ test report

function rateTable(suite) {
  const table = el("table", { class: "rates" });
  table.append(el("thead", {}, el("tr", {},
    el("th", { text: "Feature" }), el("th", { text: "Files" }), el("th", { text: "Passed" }),
    el("th", { text: "Failed" }), el("th", { text: "Rate" }), el("th", { text: "" }))));
  const body = el("tbody");
  const row = (c, cls) => {
    const bar = el("div", { class: c.fail ? "bar bad" : "bar" }, el("span"));
    bar.firstChild.style.width = `${c.pass_rate}%`;
    return el("tr", { class: cls || "" },
      el("td", { text: c.name }), el("td", { text: fmtInt(c.files) }),
      el("td", { text: fmtInt(c.pass) }), el("td", { text: fmtInt(c.fail) }),
      el("td", { text: `${c.pass_rate.toFixed(2)} %` }), el("td", { class: "barcell" }, bar));
  };
  for (const c of suite.categories) body.append(row(c));
  body.append(row({ name: "Total", files: suite.files, ...suite.total }, "total"));
  table.append(body);
  return table;
}

const SUITE_NOTES = {
  "sqllogictest full corpus":
    "Every file of SQLite's sqllogictest corpus. Records that do not apply to SQLite (skipif sqlite / onlyif another engine) are not counted.",
  "sqllogictest sample":
    "The 49-file sample CI runs on every push (which files and why: tests/slt/MANIFEST.md).",
  differential:
    "The same queries run through sqlite3 and Little SQL, answers compared. One row per feature file; known gaps still count as failures.",
};

async function loadTests() {
  const box = $("#tests");
  try {
    const data = await (await fetch("results.json")).json();
    $("#tests-summary").textContent =
      `Measured against ${data.reference} on ${data.date}. Any error, crash, timeout or unsupported feature counts as a failure; rates are rounded down. ` +
      `In read mode the real sqlite3 runs each file's CREATE and INSERT statements, and those are never counted as passes; in native mode Little SQL runs everything itself.`;
    for (const s of data.suites) {
      const name = s.mode === "native" || s.suite === "differential" ? s.suite : s.suite;
      const title = `${s.suite}${s.suite === "differential" ? "" : `, ${s.mode} mode`}`;
      box.append(el("div", { class: "suite" },
        el("h3", { text: title === "differential" ? "Differential tests" : title }),
        el("p", { class: "muted", text: SUITE_NOTES[name] || "" }),
        rateTable(s)));
    }
  } catch (e) {
    $("#tests-summary").textContent = `The test report could not be loaded (${e.message}).`;
  }
}

// ------------------------------------------------------------ start

$("#sql").value = EXAMPLES[0].sql;
loadTests();
openDemo().then(() => run());
