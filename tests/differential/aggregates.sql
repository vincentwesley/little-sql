-- fixture: shop
-- Aggregates, GROUP BY, HAVING, DISTINCT aggregates, bare columns.
SELECT count(*), count(email), count(DISTINCT city), count(credit) FROM customers;
SELECT sum(credit), avg(credit), min(credit), max(credit), total(credit) FROM customers;
SELECT city, count(*) FROM customers GROUP BY city;
SELECT city, count(*) AS n, round(avg(credit), 4) FROM customers GROUP BY city HAVING n > 40 ORDER BY n DESC, city;
SELECT status, count(*), min(placed), max(placed) FROM orders GROUP BY status ORDER BY status;
SELECT customer_id, count(*) FROM orders GROUP BY customer_id HAVING count(*) >= 10 ORDER BY customer_id;
SELECT category, sum(price), group_concat(sku, ';') FROM (SELECT * FROM products ORDER BY sku) GROUP BY category ORDER BY category;
SELECT category, count(DISTINCT weight), sum(DISTINCT price) FROM products GROUP BY category;
SELECT max(price), sku FROM products;
SELECT min(price), sku FROM products WHERE category = 'veg';
SELECT count(*) FROM products WHERE price > 1000;
SELECT sum(price), avg(price), max(price), group_concat(sku) FROM products WHERE price > 1000;
SELECT sku, sum(qty) FROM lines GROUP BY sku HAVING sum(qty) > 520 ORDER BY 2 DESC, 1;
SELECT order_id % 10 AS bucket, count(*), sum(qty) FROM lines GROUP BY bucket ORDER BY bucket;
SELECT count(*) FROM (SELECT DISTINCT customer_id FROM orders);
SELECT length(group_concat(DISTINCT status)) FROM orders;
SELECT city, max(credit) - min(credit) FROM customers WHERE credit IS NOT NULL GROUP BY 1 ORDER BY 1;
SELECT typeof(sum(id)), typeof(avg(id)), typeof(total(id)), sum(id) FROM customers;
SELECT count(*), sum(qty) FILTER (WHERE qty > 5) FROM lines;
SELECT substr(placed, 1, 7) AS month, count(*) FROM orders GROUP BY month ORDER BY month LIMIT 6;
SELECT status, customer_id IS NULL, count(*) FROM orders GROUP BY 1, 2 ORDER BY 1, 2;
SELECT count(*) AS c FROM orders GROUP BY customer_id ORDER BY c DESC, customer_id LIMIT 3;
SELECT customer_id, total, n_lines FROM order_totals WHERE total > 450 ORDER BY total DESC, order_id LIMIT 10;
