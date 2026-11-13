-- fixture: shop
-- Scalar, IN, EXISTS and correlated subqueries; FROM-subqueries; CTEs.
SELECT id, (SELECT count(*) FROM orders o WHERE o.customer_id = c.id) FROM customers c WHERE id <= 8 ORDER BY id;
SELECT id FROM customers WHERE id IN (SELECT customer_id FROM orders WHERE status = 'cancelled' AND id < 100) ORDER BY id;
SELECT id FROM customers WHERE id NOT IN (SELECT customer_id FROM orders) ORDER BY id;
SELECT id FROM customers WHERE id NOT IN (SELECT customer_id FROM orders WHERE customer_id IS NOT NULL) ORDER BY id;
SELECT id FROM customers c WHERE NOT EXISTS (SELECT 1 FROM orders o WHERE o.customer_id = c.id) ORDER BY id;
SELECT (SELECT max(price) FROM products), (SELECT sku FROM products ORDER BY price DESC, sku LIMIT 1);
SELECT (SELECT name FROM customers WHERE id = 99999);
SELECT sku, price FROM products WHERE price > (SELECT avg(price) FROM products) ORDER BY price, sku;
SELECT sku FROM products p WHERE price = (SELECT max(price) FROM products p2 WHERE p2.category IS p.category) ORDER BY 1;
SELECT city, n FROM (SELECT city, count(*) AS n FROM customers GROUP BY city) WHERE n > 42 ORDER BY city;
SELECT * FROM (SELECT id, credit FROM customers ORDER BY credit DESC LIMIT 5) ORDER BY id;
WITH big AS (SELECT customer_id, count(*) n FROM orders GROUP BY customer_id) SELECT max(n), min(n) FROM big;
WITH a(x) AS (SELECT 1 UNION ALL SELECT 2), b(y) AS (SELECT x * 10 FROM a) SELECT x, y FROM a, b ORDER BY x, y;
WITH c AS (SELECT * FROM customers WHERE city = 'Bedok') SELECT count(*), (SELECT count(*) FROM c WHERE credit > 50) FROM c;
SELECT id, (SELECT group_concat(sku) FROM (SELECT sku FROM lines WHERE order_id = o.id ORDER BY sku)) FROM orders o WHERE id <= 5 ORDER BY id;
SELECT 1 IN (SELECT 1), 2 IN (SELECT 1 UNION SELECT NULL), EXISTS (SELECT 1 WHERE 0), NOT EXISTS (SELECT 1 WHERE 0);
SELECT (SELECT 1, 2) IS NULL;
SELECT id FROM orders WHERE customer_id = (SELECT id FROM customers WHERE email = 'c5@example.com') ORDER BY id;
SELECT c.id, (SELECT max(o.placed) FROM orders o WHERE o.customer_id = c.id AND o.status = 'shipped') FROM customers c WHERE c.id BETWEEN 40 AND 45 ORDER BY 1;
SELECT count(*) FROM lines WHERE sku IN (SELECT sku FROM products WHERE category = 'fruit') AND order_id IN (SELECT id FROM orders WHERE status = 'pending');
