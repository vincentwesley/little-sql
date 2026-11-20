-- A write workload run by both Little SQL and sqlite3 (scripts/write-check.sh).
-- The two resulting files must dump identically, and sqlite3 must find
-- Little SQL's file sound.
CREATE TABLE people(id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, email TEXT UNIQUE, born INT, note BLOB);
CREATE TABLE tags(person INT, tag TEXT COLLATE NOCASE, weight REAL, PRIMARY KEY(person, tag)) WITHOUT ROWID;
CREATE INDEX people_born ON people(born DESC, name);
CREATE TABLE IF NOT EXISTS people(x);
WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 2500)
INSERT INTO people(name, email, born, note)
SELECT 'person ' || i,
       CASE WHEN i % 13 = 0 THEN NULL ELSE 'p' || i || '@example.com' END,
       1950 + i % 60,
       CASE WHEN i % 97 = 0 THEN zeroblob(5000 + i) WHEN i % 5 = 0 THEN x'00ff10' ELSE NULL END
FROM n;
WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 4000)
INSERT OR IGNORE INTO tags SELECT i % 700, 'tag' || (i * 7 % 23), i / 3.0 FROM n;
UPDATE people SET born = born + 1, name = upper(name) WHERE id % 3 = 0;
DELETE FROM people WHERE id % 7 = 0 OR born < 1955;
INSERT OR REPLACE INTO people(id, name, email, born) VALUES (5, 'replaced', 'p10@example.com', 2000);
UPDATE tags SET weight = -weight WHERE tag = 'TAG3';
DELETE FROM tags WHERE person BETWEEN 100 AND 300;
BEGIN;
INSERT INTO people(name, born) VALUES ('in txn', 1999);
CREATE TABLE scratch(a, b);
INSERT INTO scratch VALUES (1, 2);
ROLLBACK;
BEGIN;
INSERT INTO people(name, born) VALUES ('committed', 2001);
CREATE INDEX tags_weight ON tags(weight);
COMMIT;
CREATE TABLE big(id INTEGER PRIMARY KEY, body TEXT);
WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 300)
INSERT INTO big SELECT i, printf('%.*c', 100 * i, 'x') FROM n;
DELETE FROM big WHERE id % 2 = 0;
UPDATE big SET body = body || body WHERE id % 5 = 1;
CREATE TABLE temp_data AS SELECT born, count(*) AS n, max(name) FROM people GROUP BY born ORDER BY born;
CREATE VIEW recent AS SELECT name, born FROM people WHERE born > 1990;
CREATE TRIGGER people_log AFTER UPDATE OF born ON people BEGIN
  INSERT INTO temp_data(born, n) VALUES (NEW.born, -1);
END;
UPDATE people SET born = 2005 WHERE id IN (2, 4, 8);
DROP TABLE big;
DELETE FROM people WHERE name LIKE 'PERSON 1%';
INSERT INTO people(name) VALUES ('last');
CREATE TABLE checked(id INTEGER PRIMARY KEY, qty INT CHECK (qty >= 0), label TEXT, CONSTRAINT nonempty CHECK (label <> ''));
INSERT OR IGNORE INTO checked(qty, label) VALUES (1, 'a'), (-1, 'b'), (2, ''), (NULL, 'c');
UPDATE OR IGNORE checked SET qty = qty - 1;
-- Savepoints.
SAVEPOINT sp_outer;
INSERT INTO checked(qty, label) VALUES (10, 'sp');
SAVEPOINT sp_inner;
INSERT INTO checked(qty, label) VALUES (11, 'gone');
ROLLBACK TO sp_inner;
RELEASE sp_outer;
-- ALTER TABLE, rewriting indexes, triggers and views that refer to it.
ALTER TABLE people ADD COLUMN nickname TEXT DEFAULT 'none';
ALTER TABLE people RENAME COLUMN born TO birth_year;
ALTER TABLE temp_data DROP COLUMN "max(name)";
ALTER TABLE people RENAME TO persons;
UPDATE persons SET birth_year = 2006 WHERE id = 2;
INSERT INTO persons(name, birth_year) VALUES ('after alter', 1980);
