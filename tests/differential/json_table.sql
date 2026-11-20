-- fixture: shop
-- Table-valued JSON functions: json_each and json_tree in FROM.
SELECT * FROM json_each('{"a":1,"b c":[2,3.5],"d":{"e":null},"f":"x\"y","g":true}');
SELECT * FROM json_tree('{"a":1,"b c":[2,3.5],"d":{"e":null},"f":"x\"y","g":true}');
SELECT key, value, type, atom, id, parent, fullkey, path FROM json_each('[10,"x",true,false,null,[],{}]');
SELECT * FROM json_each('{"a":{"b":[1,2]}}', '$.a.b');
SELECT * FROM json_tree('{"a":{"b":[1,2]}}', '$.a');
SELECT * FROM json_tree('[1,[5,6]]', '$[1]');
SELECT * FROM json_tree('{"a":[1,2]}', '$.a[1]');
SELECT * FROM json_each('{"a":[1,2]}', '$.a[#-1]');
SELECT * FROM json_each('{"a":[1,2]}', '$.missing');
SELECT * FROM json_each('5');
SELECT * FROM json_each(NULL);
SELECT * FROM json_each('[]');
SELECT * FROM json_tree('[1,[2,[3,{"k":[4]}]]]');
SELECT fullkey FROM json_tree('{"$":1,"a.b":2,"1x":3,"_y":4,"":5,"ok9":6}');
SELECT key, json, root FROM json_each('[1]', '$');
SELECT j.value FROM json_each('[3,1,2]') AS j ORDER BY j.value DESC;
SELECT sum(value), count(*), max(key) FROM json_each('[1,2,3,4]') WHERE value > 1;
SELECT count(*) FROM json_each('[1,2]') a, json_each('[3,4,5]') b WHERE a.value < b.value;
SELECT fullkey, atom FROM json_tree('{"a":[{"b":1}],"c":"s"}') WHERE atom IS NOT NULL ORDER BY fullkey;
SELECT value, type FROM json_each(json_array(1, json_object('a', 2), 'z'));
SELECT key, value FROM json_each(json_object('id', 7, 'name', 'n')) WHERE key = 'name';
SELECT c.id, e.value FROM customers c, json_each(json_array(c.id, c.id * 10)) e WHERE c.id <= 3 ORDER BY c.id, e.value;
SELECT c.id, e.key FROM customers c LEFT JOIN json_each(CASE WHEN c.id % 2 = 0 THEN '[1]' END) e WHERE c.id <= 4 ORDER BY c.id;
SELECT c.id, (SELECT count(*) FROM json_each(json_group_array(o.id))) FROM customers c JOIN orders o ON o.customer_id = c.id WHERE c.id <= 5 GROUP BY c.id ORDER BY c.id;
SELECT e.value, count(*) FROM orders o, json_each('[1,2]') e WHERE o.id <= 4 GROUP BY e.value;
SELECT key FROM json_each('{"z":1,"a":2,"m":3}') ORDER BY rowid;
WITH docs(d) AS (VALUES ('{"tags":["red","blue"]}'), ('{"tags":["blue"]}'), ('{}'))
SELECT t.value, count(*) FROM docs, json_each(docs.d, '$.tags') t GROUP BY t.value ORDER BY 1;
SELECT * FROM json_each('not json');
SELECT * FROM json_each('[1]', 'bad path');
SELECT * FROM json_each('[1]', '$', 'extra');
