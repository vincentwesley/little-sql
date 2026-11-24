-- fixture: shop
-- JSON functions and operators.
SELECT json(' { "a" : [1, 2.50, true, null], "b" : {"c": "xé\n"} } ');
SELECT json_valid('{"a":1}'), json_valid('{a:1}'), json_valid('[1,2,]'), json_valid(NULL), json_valid(42);
SELECT json_type('{"a":[1,2.5,"s",true,false,null,{}]}', '$.a[' || n || ']') FROM (SELECT 0 AS n UNION ALL SELECT 1 UNION ALL SELECT 2 UNION ALL SELECT 3 UNION ALL SELECT 4 UNION ALL SELECT 5 UNION ALL SELECT 6) ORDER BY n;
SELECT json_type('[1]'), json_type('{"a":1}', '$.b'), json_type('"x"');
SELECT json_extract('{"a":{"b":[10,20,30]}}', '$.a.b[1]'), json_extract('{"a":{"b":[10,20,30]}}', '$.a.b[#-1]'), json_extract('{"a":{"b":[10,20,30]}}', '$.a');
SELECT json_extract('{"a":"text","b":1.50,"c":true,"d":null}', '$.a', '$.b', '$.c', '$.d', '$.zz');
SELECT typeof(json_extract('{"a":1.0}', '$.a')), typeof(json_extract('{"a":1}', '$.a')), typeof(json_extract('{"a":"1"}', '$.a'));
SELECT '{"a":{"b":"c"}}' -> '$.a', '{"a":{"b":"c"}}' -> 'a', '{"a":{"b":"c"}}' ->> '$.a.b', '[5,6,7]' -> 1, '[5,6,7]' ->> 2, '{"a":1}' -> 'missing';
SELECT json_array(1, 2.5, 'three', NULL, json_array(4, 5), json_object('k', 'v'), '[6]', json('[7]'));
SELECT json_object('id', id, 'name', name, 'city', city, 'credit', credit) FROM customers WHERE id <= 3 ORDER BY id;
SELECT json_object('nested', json_object('list', json_array(1, 2)), 'extracted', json_extract('{"x":[1]}', '$.x'), 'str', json_extract('{"x":"y"}', '$.x'));
SELECT json_array_length('[1,2,3]'), json_array_length('{"a":[1,2]}', '$.a'), json_array_length('{"a":1}'), json_array_length('[]');
SELECT json_quote('a "b"'), json_quote(3.5), json_quote(NULL), json_quote(json('[1]')), json_quote(7);
SELECT city, json_group_array(id) FROM customers WHERE id <= 12 GROUP BY city ORDER BY city;
SELECT json_group_object(sku, price) FROM products WHERE sku < 'SKU-004';
SELECT json_group_array(json_object('sku', sku, 'qty', qty)) FROM lines WHERE order_id = 7;
SELECT json_group_array(credit) FROM customers WHERE id IN (10, 11, 12);
SELECT json_array('tab	here', char(1), 'é', '"q"', '\b\');
SELECT json('[1.0, 1e5, -0.25, 12345678901234567890]'), json_array(1.0, 0.1, 1e20, -3);
SELECT name FROM customers WHERE json_object('c', city) ->> 'c' = 'Bedok' AND id < 30 ORDER BY id;
SELECT json('{"a":1,"a":2}') ->> 'a', json_extract('{"a":1,"a":2}', '$.a');
SELECT json('{bad json}');
SELECT json_extract('[1,2]', 'not a path');
SELECT json_object('a');
SELECT json_array(x'01');
