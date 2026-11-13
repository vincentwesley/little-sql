-- fixture: pages
-- 512-byte pages: deep interior levels, long overflow chains, WITHOUT ROWID.
SELECT count(*), sum(n), min(k), max(k) FROM big;
SELECT id, length(payload), substr(payload, -6) FROM big WHERE id % 100 = 0 ORDER BY id;
SELECT id, k FROM big WHERE k BETWEEN 'key-01000' AND 'key-01010' ORDER BY k;
SELECT id FROM big WHERE n = 5 AND k > 'key-02900' ORDER BY k;
SELECT n, count(*) FROM big GROUP BY n ORDER BY n DESC LIMIT 5;
SELECT * FROM big WHERE id IN (1, 1500, 2999, 3000, 3001);
SELECT max(length(payload)) FROM big;
SELECT count(*), count(DISTINCT a), min(b), max(b) FROM wr;
SELECT * FROM wr WHERE a = 'g7' AND b BETWEEN 100 AND 200 ORDER BY b;
SELECT a, count(*), sum(b) FROM wr GROUP BY a ORDER BY a;
SELECT * FROM wr ORDER BY a DESC, b DESC LIMIT 5;
SELECT big.id, wr.a FROM big JOIN wr ON wr.b = big.id WHERE big.n = 0 ORDER BY 1 LIMIT 10;
