-- Tables for planner tests: empty or tiny, so plans rest on SQLite's
-- no-statistics assumptions rather than on table sizes.
CREATE TABLE e1(a, b);
CREATE INDEX e1a ON e1(a);
CREATE TABLE e2(x INTEGER PRIMARY KEY, y);
CREATE INDEX e2y ON e2(y);
CREATE TABLE e3(k, v);
CREATE TABLE s1(a, b);
INSERT INTO s1 VALUES(1, 2), (3, 4);
CREATE INDEX s1b ON s1(b);
