-- fixture: shop
-- ORDER BY delivered by index or rowid order (forwards and backwards),
-- IN lookups, WITHOUT ROWID keys and GROUP BY over sorted input.
SELECT id FROM customers ORDER BY id DESC LIMIT 5;
SELECT id FROM customers WHERE id > 290 ORDER BY id DESC;
SELECT id FROM customers WHERE id BETWEEN 10 AND 20 ORDER BY id DESC;
SELECT id FROM customers WHERE id < 5 ORDER BY rowid DESC;
SELECT id, city FROM customers ORDER BY city, id LIMIT 12;
SELECT id, city FROM customers ORDER BY city DESC, id DESC LIMIT 12;
SELECT city FROM customers WHERE city > 'C' ORDER BY city DESC LIMIT 7;
SELECT id, placed FROM orders WHERE customer_id = 9 ORDER BY placed;
SELECT id, placed FROM orders WHERE customer_id = 9 ORDER BY placed DESC LIMIT 3;
SELECT customer_id, placed FROM orders WHERE customer_id BETWEEN 3 AND 5 ORDER BY customer_id DESC, placed;
SELECT id FROM customers WHERE id IN (5, 3, 3, 1, 999, NULL) ORDER BY id;
SELECT id FROM customers WHERE id IN (5, 3, 1) ORDER BY id DESC;
SELECT id FROM customers WHERE id IN ('5', 2.0, 7.5) ORDER BY id;
SELECT id, city FROM customers WHERE city IN ('Punggol', 'Bedok', 'Nowhere') ORDER BY city, id;
SELECT id FROM orders WHERE customer_id IN (SELECT id FROM customers WHERE city = 'Clementi') AND id < 100 ORDER BY id;
SELECT id FROM customers WHERE id IN (SELECT customer_id FROM orders WHERE id < 10) ORDER BY id;
SELECT * FROM lines WHERE order_id = 10 ORDER BY sku DESC;
SELECT * FROM lines WHERE order_id IN (10, 11) ORDER BY order_id DESC, sku DESC;
SELECT * FROM lines WHERE order_id > 1995 ORDER BY order_id, sku;
SELECT order_id, sku FROM lines WHERE sku = 'SKU-002' ORDER BY order_id LIMIT 5;
SELECT order_id, qty FROM lines WHERE sku IN ('SKU-002', 'SKU-003') AND order_id < 200 ORDER BY sku, order_id;
SELECT order_id, sku, qty FROM lines ORDER BY order_id DESC, sku DESC LIMIT 4;
SELECT city, count(*), min(id), max(id) FROM customers GROUP BY city;
SELECT city, count(*) FROM customers WHERE city > 'B' GROUP BY city HAVING count(*) > 42;
SELECT city, count(*) FROM customers GROUP BY city ORDER BY 2 DESC, 1 LIMIT 3;
SELECT order_id, count(*), sum(qty) FROM lines WHERE order_id < 30 GROUP BY order_id;
SELECT customer_id, count(*) FROM orders GROUP BY customer_id LIMIT 5;
SELECT city, name FROM customers GROUP BY city;
SELECT category, max(price), sku FROM products GROUP BY category;
SELECT sku FROM products INDEXED BY products_cat_price WHERE category = 'veg' ORDER BY price;
SELECT count(*) FROM customers NOT INDEXED WHERE city = 'Bedok';
