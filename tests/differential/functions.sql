-- fixture: types
-- Scalar functions on every storage class.
SELECT id, length(tx), length(b), upper(tx), lower(tx), hex(b) FROM t ORDER BY id;
SELECT id, abs(i), abs(r), round(r), round(r, 1), sign(r) FROM t WHERE id <> 5 ORDER BY id;
SELECT id, substr(tx, 2), substr(tx, 2, 1), substr(tx, -2), substr(tx, 0, 2), substring(tx, 1, 3) FROM t ORDER BY id;
SELECT id, trim(tx), ltrim(tx), rtrim(tx), trim(tx, 'a'), replace(tx, 'a', 'A') FROM t ORDER BY id;
SELECT id, instr(tx, 'b'), instr(b, x'ff'), quote(tx), quote(b), quote(r) FROM t ORDER BY id;
SELECT id, ifnull(i, 'none'), nullif(i, 42), coalesce(i, r, tx), iif(i > 0, 'y', 'n') FROM t ORDER BY id;
SELECT id, unicode(tx), char(65, 66, 67), printf('%5.2f|%d|%s|%x', r, i, tx, 255) FROM t WHERE id IN (2, 3, 10) ORDER BY id;
SELECT max(1, 2, 3), min('a', 'b'), max(1, 'a', x'00'), min(NULL, 1), max(2.5, 2);
SELECT typeof(zeroblob(4)), length(zeroblob(4)), hex(zeroblob(2)), typeof(randomblob(4)), length(randomblob(7));
SELECT likely(1), unlikely(0), likelihood(1, 0.5), typeof(NULL), sqlite_version() IS NOT NULL;
SELECT round(2.5), round(-2.5), round(0.5), round(1.005, 2), round(1e20), round(123.456, -1);
SELECT format('%s-%d', 'a', 3), printf('%05d', -42), printf('%.3s', 'abcdef'), printf('%10s|%-10s|', 'r', 'l'), printf('%%');
SELECT printf('%e', 12345.678), printf('%g', 0.0001), printf('%g', 1e20), printf('%c', 'xyz'), printf('%q', 'it''s');
SELECT hex('é'), length('é'), octet_length('é'), unicode('é'), char(233);
SELECT id, glob('*a*', tx), like('%b%', tx) FROM t ORDER BY id;
SELECT trim('  x  '), ltrim('xxyy', 'x'), rtrim('xxyy', 'y'), replace('aaa', 'a', ''), replace('abc', '', 'x');
SELECT upper('ß'), lower('ÀÉ'), length(CAST(x'00410042' AS TEXT)), instr('abc', ''), substr('abc', 5);
SELECT concat('a', NULL, 1, 2.5), concat_ws('-', 'a', NULL, 'b'), 'x' || 1 || 2.0;
SELECT abs(-9223372036854775807), abs(NULL), abs('-3'), abs('x');
SELECT total_changes() >= 0, changes() >= 0, last_insert_rowid() >= 0;
