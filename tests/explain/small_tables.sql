-- fixture: planner
-- Plans over empty and tiny tables must match sqlite3's line for line.
SELECT * FROM e1 WHERE a = 1;
SELECT * FROM e1 WHERE a > 1;
SELECT a FROM e1 WHERE a = 1;
SELECT * FROM e1 WHERE b = 1;
SELECT * FROM e2 WHERE y = 3;
SELECT * FROM e2 WHERE x = 3;
SELECT * FROM e1, e2 WHERE e1.a = e2.x;
SELECT * FROM e1, e3 WHERE e1.a = e3.k;
SELECT * FROM s1 WHERE b = 2;
SELECT * FROM s1, e1 WHERE s1.a = e1.a;
SELECT * FROM e1 ORDER BY a;
SELECT * FROM e1 WHERE a IN (1,2,3);
SELECT count(*) FROM e1 GROUP BY a;
SELECT * FROM e1 WHERE a = 1 OR b = 2;
SELECT * FROM e3 WHERE k = 1 ORDER BY v;
