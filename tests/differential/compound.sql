-- fixture: shop
-- UNION, UNION ALL, INTERSECT, EXCEPT and VALUES.
SELECT city FROM customers UNION SELECT status FROM orders;
SELECT city FROM customers WHERE id < 10 UNION ALL SELECT status FROM orders WHERE id < 5;
SELECT customer_id FROM orders INTERSECT SELECT id FROM customers WHERE city = 'Bedok';
SELECT id FROM customers EXCEPT SELECT customer_id FROM orders;
SELECT 1 UNION SELECT 1.0 UNION SELECT '1' UNION SELECT NULL;
SELECT id, 'c' FROM customers WHERE id < 3 UNION ALL SELECT id, 'o' FROM orders WHERE id < 3 ORDER BY 2, 1;
SELECT name FROM customers WHERE id < 5 UNION SELECT title FROM products WHERE price < 3 ORDER BY 1 DESC LIMIT 4;
VALUES (1, 'a'), (2, 'b'), (3, NULL);
SELECT * FROM (VALUES (3), (1), (2)) ORDER BY 1;
SELECT column1, column2 FROM (VALUES (1, 2), (3, 4)) WHERE column1 > 1;
SELECT a FROM (SELECT 1 AS a UNION SELECT 2 EXCEPT SELECT 1);
SELECT x FROM (SELECT 1 x UNION ALL SELECT 1 UNION ALL SELECT 2) GROUP BY x HAVING count(*) > 1;
SELECT title COLLATE NOCASE FROM products WHERE sku < 'SKU-005' UNION SELECT upper(title) FROM products WHERE sku < 'SKU-005';
SELECT count(*) FROM (SELECT sku FROM lines UNION SELECT sku FROM products);
