-- fixture: types
-- Arithmetic, comparison, affinity, NULL logic, CASE and CAST.
SELECT id, i + 1, r * 2, n - 1, tx || '!' FROM t ORDER BY id;
SELECT id, i / 2, i % 3, -i FROM t WHERE i IS NOT NULL AND abs(i) < 1000000 ORDER BY id;
SELECT id, typeof(i), typeof(r), typeof(n), typeof(tx), typeof(b), typeof(any_) FROM t ORDER BY id;
SELECT id, i = '7', n = 1, tx = 10, any_ = 12 FROM t ORDER BY id;
SELECT id, i < r, n > tx, b = 'not a blob' FROM t ORDER BY id;
SELECT id, i IS NULL, i IS NOT NULL, i IS 42, i IS NOT 42 FROM t ORDER BY id;
SELECT id, NULL AND 0, NULL OR 1, NOT i, i AND r FROM t ORDER BY id;
SELECT id, CASE WHEN i > 0 THEN 'pos' WHEN i < 0 THEN 'neg' ELSE 'zero-or-null' END FROM t ORDER BY id;
SELECT id, CASE i WHEN 1 THEN 'one' WHEN 42 THEN 'answer' END FROM t ORDER BY id;
SELECT id, CAST(n AS INTEGER), CAST(n AS REAL), CAST(n AS TEXT), CAST(tx AS NUMERIC) FROM t ORDER BY id;
SELECT id, CAST(b AS TEXT), CAST(r AS INTEGER) FROM t WHERE id IN (3, 4, 9) ORDER BY id;
SELECT 9223372036854775807 + 1, -9223372036854775808 - 1, 9223372036854775807 * 2;
SELECT 7 / 2, 7.0 / 2, 7 % -3, -7 % 3, 1 / 0, 1 % 0, 5.5 % 2;
SELECT 1 << 62, 1 << 63, 1 << 64, -1 >> 1, 5 & 3, 5 | 3, ~5;
SELECT '3' + '4', '3.5' * 2, 'abc' + 1, x'31' + 1, '0x10' + 0;
SELECT 0.1 + 0.2, 1e308 * 10, -1e308 * 10, 1.0 / 3, 2.0 / 3 * 3;
SELECT 1 = 1.0, '1' = 1, '1' = '1.0', 'a' < 'b', 'a' < 'B', x'00' < 'a', NULL = NULL;
SELECT 1 IN (1, 2, NULL), 3 IN (1, 2, NULL), 3 NOT IN (1, 2, NULL), NULL IN (), 1 NOT IN ();
SELECT 5 BETWEEN 1 AND 10, 'm' BETWEEN 'a' AND 'z', NULL BETWEEN 1 AND 2, 3 NOT BETWEEN 4 AND 2;
SELECT 'abc' LIKE 'A%', 'abc' LIKE 'a_c', 'a%c' LIKE 'a\%c' ESCAPE '\', 'ABC' GLOB 'a*', 'x' NOT LIKE '%';
SELECT 'Hello' COLLATE NOCASE = 'hello', 'a ' = 'a' COLLATE RTRIM, 'x' || NULL, coalesce(NULL, NULL, 3);
SELECT id, r FROM t WHERE r > 0 ORDER BY r;
SELECT id FROM t WHERE tx > 5 ORDER BY id;
SELECT id FROM t WHERE n BETWEEN 0 AND 10 ORDER BY id;
SELECT id, -0.0, 0.0 = -0.0, 1e15, 1e16, 123456789.123456789, 1.5e-7 FROM t WHERE id = 1;
SELECT 10 - 2 - 3, 2 * 3 + 4, 2 + 3 * 4, 2 * (3 + 4), -2 * -3, - - 1, + 'x';
