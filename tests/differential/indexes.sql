-- fixture: shop
-- Queries whose best plan uses an index; results must match a full scan.
SELECT id, placed FROM orders WHERE customer_id = 17 ORDER BY placed DESC;
SELECT id FROM orders WHERE customer_id = 17 AND placed > '2023-06-01' ORDER BY id;
SELECT id FROM orders WHERE customer_id IN (5, 6, 7) ORDER BY id;
SELECT id FROM orders WHERE customer_id BETWEEN 290 AND 300 ORDER BY customer_id, placed DESC;
SELECT id, city FROM customers WHERE city >= 'P' ORDER BY city, id;
SELECT sku, price FROM products WHERE category = 'fruit' AND price BETWEEN 10 AND 40 ORDER BY price;
SELECT sku FROM products WHERE category = 'veg' ORDER BY price DESC LIMIT 3;
SELECT count(*) FROM lines WHERE sku = 'SKU-010';
SELECT order_id FROM lines WHERE sku = 'SKU-010' AND order_id > 1900 ORDER BY order_id;
SELECT id FROM customers WHERE email = 'c123@example.com';
SELECT id FROM customers WHERE email > 'c99' ORDER BY email;
SELECT id FROM orders WHERE id > 1995;
SELECT id FROM orders WHERE id >= 10 AND id < 13 ORDER BY id DESC;
SELECT id FROM orders WHERE rowid IN (3, 1, 2) ORDER BY id;
SELECT id FROM customers WHERE city = 'Bedok' AND id > 280 ORDER BY id;
SELECT DISTINCT customer_id FROM orders WHERE customer_id < 6 ORDER BY 1;
SELECT min(placed), max(placed) FROM orders WHERE customer_id = 42;
SELECT customer_id, max(placed) FROM orders WHERE customer_id <= 3 GROUP BY customer_id;
SELECT o.id, c.name FROM customers c JOIN orders o ON o.customer_id = c.id WHERE c.city = 'Clementi' AND o.id < 60 ORDER BY o.id;
SELECT p.sku, count(*) FROM products p JOIN lines l ON l.sku = p.sku WHERE p.category = 'fruit' GROUP BY p.sku ORDER BY p.sku;
SELECT id FROM customers WHERE city = 'bedok';
SELECT sku FROM products WHERE title = 'banana 5';
SELECT sku FROM products WHERE price = '10';
SELECT sku FROM products WHERE category IS NULL AND price < 5 ORDER BY sku;
