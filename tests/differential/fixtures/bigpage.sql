-- The largest page size, with long text that still overflows.
PRAGMA page_size = 65536;
CREATE TABLE docs(id INTEGER PRIMARY KEY, title TEXT, body TEXT);
WITH RECURSIVE s(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM s WHERE i < 200)
INSERT INTO docs SELECT i, 'doc ' || i, printf('%.*c', 150 * i, 'z') FROM s;
CREATE INDEX docs_title ON docs(title);
