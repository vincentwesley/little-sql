-- fixture: types
-- JSON5 input, string escapes kept as written, json_valid flags and
-- json_error_position.
SELECT json('{a:1, $b_2:[1,2,], "c":{d:'x', e:"it''s"},}');
SELECT json('[.5, 5., +3, -.25, 1.e3, 0x1F, -0x10, 0XFF, Infinity, -Infinity, +inf, NaN, QNaN, nan]');
SELECT json('// comment
[1, /* inline */ 2]'), json(char(9) || char(11) || char(12) || '[1' || char(160) || ']' || char(65279));
SELECT json('["a\/b", "\u0041", "\u000a", "\t", "\x41", "\v", "\0", "it\''s", "a\
b"]');
SELECT json('{\u0061:1, a\u0062:2}');
SELECT json('[0xFFFFFFFFFFFFFFFF]'), json('[-0x8000000000000000]'), json('[0x1FFFFFFFFFFFFFFFF]');
SELECT json_extract('{a:[1,0x10,]}', '$.a[1]'), json_type('[0x10]', '$[0]'), json_type('[.5]', '$[0]'), json_extract('[Infinity]', '$[0]');
SELECT json_valid('{a:1}'), json_valid('{a:1}', 1), json_valid('{a:1}', 2), json_valid('{a:1}', 3), json_valid('[1]', 4), json_valid(5), json_valid(NULL), json_valid(2.5, 1);
SELECT json_valid(x'0c', 4), json_valid(x'0c', 8), json_valid(x'0c', 1), json_valid(x'1b02', 8), json_valid(x'2b3131', 4), json_valid(x'2b3131', 8), json_valid(jsonb_like, 8) FROM (SELECT x'ff' AS jsonb_like);
SELECT json_valid('[1]', 0);
SELECT json_valid('[1]', 16);
SELECT json_error_position('{"a":1'), json_error_position('[1,2'), json_error_position('[1]'), json_error_position('{"a" 1}'), json_error_position('[1,]'), json_error_position('[1 2]'), json_error_position('["ab'), json_error_position('[tru]'), json_error_position('{"a":}');
SELECT json_error_position(''), json_error_position(' '), json_error_position('[1]x'), json_error_position('["é", x]'), json_error_position('[00]'), json_error_position('[-]'), json_error_position('[1e]'), json_error_position('[0x]'), json_error_position(NULL), json_error_position(5);
SELECT json_error_position('{1:2}'), json_error_position('{ [1,2]:2}'), json_error_position('{"x": 1, ["b c": 1}'), json_error_position('{a:1 , ,}'), json_error_position('[1 , ,]'), json_error_position('{  "a"  :  }'), json_error_position('["a",  '), json_error_position('{"a":1,  ');
SELECT json_error_position('["a\qb"]'), json_error_position('["a' || char(9) || 'b"]'), json_error_position('{k: \/}'), json_error_position('[Infx]');
SELECT json('["\u0041"]') ->> 0, json_extract('{"\u0061":1}', '$.a'), json('{"\u0061":1}') -> 'a';
SELECT key, value, id, fullkey FROM json_tree('{"\u0061":"\n", "a\"b":[1], "\/":"\u00e9"}');
SELECT json_set('{"\u0061":1}', '$.a', 2), json_remove('{"\u0061":1}', '$.a'), json_patch('{"\u0061":1}', '{"a":2}'), json_insert('{"a":1}', '$."a b"', 'q"r');
SELECT json_object('k' || char(10), 'v' || char(1)), json_array('\', '"'), json_group_object(t, i) FROM (SELECT 'a"b' AS t, 1 AS i UNION ALL SELECT 'c\d', 2);
SELECT json('{"a":1}') = '{"a":1}', json('[ "x" ]') = '["x"]', length(json('"\u00e9"'));
SELECT json('[1,,2]');
SELECT json('{,}');
SELECT json('[-nan]');
