-- fixture: shop
-- Views and FROM-subqueries, flattened or materialized.
SELECT * FROM pending_orders WHERE id < 40 ORDER BY id;
SELECT city, count(*) FROM pending_orders GROUP BY city ORDER BY city;
SELECT customer, placed FROM pending_orders WHERE customer_id = 17 ORDER BY placed;
SELECT p.id, l.sku FROM pending_orders p JOIN lines l ON l.order_id = p.id WHERE p.city = 'Bedok' ORDER BY 1, 2 LIMIT 15;
SELECT count(*) FROM pending_orders p WHERE p.id IN (SELECT order_id FROM lines WHERE qty > 8);
SELECT x.name, x.n FROM (SELECT name, credit * 2 AS n FROM customers WHERE credit > 100) AS x WHERE x.n < 220 ORDER BY 1;
SELECT * FROM (SELECT id, city FROM customers) WHERE city = 'Punggol' ORDER BY id LIMIT 5;
SELECT a.id, b.id FROM (SELECT id, city FROM customers WHERE id < 20) a JOIN (SELECT id, city FROM customers WHERE id > 280) b ON a.city = b.city ORDER BY 1, 2;
SELECT c.name, t.total FROM customers c JOIN order_totals t ON t.customer_id = c.id WHERE c.id < 4 ORDER BY t.order_id;
SELECT DISTINCT city FROM pending_orders ORDER BY 1;
SELECT max(placed), min(customer) FROM pending_orders;
SELECT (SELECT count(*) FROM pending_orders p WHERE p.customer_id = c.id) AS n, c.id FROM customers c WHERE c.id <= 6 ORDER BY c.id;
SELECT * FROM pending_orders p LEFT JOIN lines l ON l.order_id = p.id WHERE p.id < 12 ORDER BY p.id, l.sku;
SELECT * FROM customers c LEFT JOIN pending_orders p ON p.customer_id = c.id WHERE c.id < 4 ORDER BY c.id, p.id;
SELECT customer FROM pending_orders UNION SELECT name FROM customers WHERE id < 3 ORDER BY 1 LIMIT 5;
