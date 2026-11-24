-- Every storage class and affinity, edge-case numbers and strings.
CREATE TABLE t(
  id INTEGER PRIMARY KEY,
  i INTEGER, r REAL, n NUMERIC, tx TEXT, b BLOB, any_
);
INSERT INTO t VALUES
 (1, 0, 0.0, 0, '', x'', NULL),
 (2, 1, 1.5, '1.0', 'abc', x'00ff', 1),
 (3, -1, -2.25, '12abc', 'ABC', x'414243', 'text'),
 (4, 9223372036854775807, 1e308, '9223372036854775808', '  42  ', x'deadbeef', 3.0),
 (5, -9223372036854775808, -1e-300, '-0', '0x1F', NULL, x'01'),
 (6, NULL, NULL, NULL, NULL, NULL, NULL),
 (7, '7', '8', ' 9 ', 10, 11, '12'),
 (8, 2147483648, 3.0, 1e3, 'émoji 🎉', 'not a blob', -0.0),
 (9, 123456789012, 0.1, '1e400', 'tab	here', x'0a', 'MiXeD'),
 (10, 42, 2.5, 3.14159, 'a''quote', x'7f', 1e15);
