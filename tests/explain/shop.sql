-- fixture: shop
-- Each query's EXPLAIN QUERY PLAN must match sqlite3's line for line.
SELECT * FROM customers WHERE id = 5;
SELECT * FROM customers WHERE id BETWEEN 5 AND 9;
SELECT * FROM customers WHERE city = 'Bedok';
SELECT name FROM customers WHERE email = 'c5@example.com';
SELECT id FROM orders WHERE customer_id = 7;
SELECT id FROM orders WHERE customer_id = 7 AND placed > '2023-06-01';
SELECT * FROM orders WHERE customer_id = 7 ORDER BY placed DESC;
SELECT * FROM products WHERE sku = 'SKU-001';
SELECT sku FROM products WHERE category = 'fruit' AND price > 10;
SELECT * FROM lines WHERE order_id = 10;
SELECT order_id FROM lines WHERE sku = 'SKU-002';
SELECT count(*) FROM customers;
SELECT * FROM customers ORDER BY id DESC;
SELECT o.id, c.name FROM orders o JOIN customers c ON c.id = o.customer_id WHERE c.city = 'Bedok';
SELECT c.name, o.id FROM customers c JOIN orders o ON o.customer_id = c.id WHERE c.id = 3;
SELECT l.qty, p.title FROM lines l JOIN products p ON p.sku = l.sku WHERE l.order_id = 99;
SELECT * FROM customers WHERE id IN (1, 2, 3);
SELECT city, count(*) FROM customers GROUP BY city;
