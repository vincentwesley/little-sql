-- A tiny page size forces deep b-trees and overflow chains.
PRAGMA page_size = 512;
CREATE TABLE big(id INTEGER PRIMARY KEY, k TEXT, payload TEXT, n INTEGER);
CREATE INDEX big_k ON big(k);
CREATE INDEX big_n_k ON big(n, k);
WITH RECURSIVE s(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM s WHERE i < 3000)
INSERT INTO big SELECT i, printf('key-%05d', (i * 7) % 3000),
  CASE WHEN i % 100 = 0 THEN substr(replace(hex(zeroblob(2000)), '0', 'x') || i, 1, 3000 + i % 7) ELSE 'p' || i END,
  i % 37 FROM s;
CREATE TABLE wr(a TEXT, b INTEGER, c, PRIMARY KEY(a, b)) WITHOUT ROWID;
WITH RECURSIVE s(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM s WHERE i < 1500)
INSERT INTO wr SELECT 'g' || (i % 13), i, 'v' || substr('abcdefghij', 1 + i % 10) FROM s;
