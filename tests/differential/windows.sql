-- fixture: shop
-- Window functions: ranking, offsets, values, aggregates over frames.
SELECT id, city, row_number() OVER (PARTITION BY city ORDER BY id) FROM customers WHERE id <= 30 ORDER BY id;
SELECT id, credit, rank() OVER (ORDER BY credit), dense_rank() OVER (ORDER BY credit) FROM customers WHERE id <= 40 ORDER BY id;
SELECT id, round(credit) AS c, rank() OVER w, dense_rank() OVER w, percent_rank() OVER w, cume_dist() OVER w FROM customers WHERE id <= 40 WINDOW w AS (ORDER BY round(credit) / 20) ORDER BY id;
SELECT id, ntile(4) OVER (ORDER BY id), ntile(7) OVER (ORDER BY id DESC) FROM customers WHERE id <= 23 ORDER BY id;
SELECT id, lag(credit) OVER (ORDER BY id), lead(credit, 2) OVER (ORDER BY id), lag(credit, 3, -1) OVER (ORDER BY id) FROM customers WHERE id <= 12 ORDER BY id;
SELECT id, first_value(name) OVER (PARTITION BY city ORDER BY id), last_value(name) OVER (PARTITION BY city ORDER BY id), nth_value(id, 2) OVER (PARTITION BY city ORDER BY id) FROM customers WHERE id <= 25 ORDER BY id;
SELECT id, last_value(id) OVER (PARTITION BY city ORDER BY id ROWS BETWEEN UNBOUNDED PRECEDING AND UNBOUNDED FOLLOWING) FROM customers WHERE id <= 25 ORDER BY id;
SELECT id, sum(id) OVER (ORDER BY id), avg(id) OVER (ORDER BY id ROWS BETWEEN 1 PRECEDING AND 1 FOLLOWING), count(*) OVER (ORDER BY id ROWS 2 PRECEDING) FROM customers WHERE id <= 15 ORDER BY id;
SELECT id, sum(qty) OVER (PARTITION BY order_id), count(*) OVER (PARTITION BY order_id), max(qty) OVER (PARTITION BY order_id ORDER BY sku) FROM lines JOIN orders o ON o.id = lines.order_id WHERE order_id <= 6 ORDER BY order_id, sku;
SELECT order_id, sku, sum(qty) OVER (ORDER BY order_id GROUPS BETWEEN 1 PRECEDING AND CURRENT ROW) FROM lines WHERE order_id <= 8 ORDER BY order_id, sku;
SELECT order_id, sku, sum(qty) OVER (ORDER BY order_id GROUPS BETWEEN CURRENT ROW AND 1 FOLLOWING) FROM lines WHERE order_id <= 8 ORDER BY order_id, sku;
SELECT order_id, sku, sum(qty) OVER (ORDER BY order_id RANGE BETWEEN CURRENT ROW AND UNBOUNDED FOLLOWING) FROM lines WHERE order_id <= 6 ORDER BY order_id, sku;
SELECT id, city, count(*) OVER (PARTITION BY city), group_concat(id, ';') OVER (PARTITION BY city ORDER BY id DESC) FROM customers WHERE id <= 20 ORDER BY id;
SELECT id, credit, rank() OVER (ORDER BY credit DESC NULLS LAST), rank() OVER (ORDER BY credit NULLS FIRST) FROM customers WHERE id BETWEEN 40 AND 60 ORDER BY id;
SELECT city, count(*) AS n, rank() OVER (ORDER BY count(*) DESC), sum(count(*)) OVER () FROM customers GROUP BY city ORDER BY city;
SELECT status, count(*), sum(count(*)) OVER (ORDER BY status) FROM orders GROUP BY status ORDER BY status;
SELECT id, sum(qty) FILTER (WHERE qty > 4) OVER (ORDER BY id) FROM (SELECT order_id AS id, qty FROM lines WHERE order_id <= 10 GROUP BY order_id) ORDER BY id;
SELECT * FROM (SELECT id, city, row_number() OVER (PARTITION BY city ORDER BY credit DESC, id) AS r FROM customers) WHERE r <= 2 ORDER BY city, r;
SELECT DISTINCT city, count(*) OVER (PARTITION BY city) FROM customers ORDER BY 1;
SELECT id, row_number() OVER (), count(*) OVER () FROM customers WHERE id <= 5 ORDER BY id;
SELECT id, min(credit) OVER win, max(credit) OVER win FROM customers WHERE id <= 12 WINDOW win AS (ORDER BY id ROWS BETWEEN 2 PRECEDING AND CURRENT ROW) ORDER BY id;
SELECT id, total(credit) OVER (PARTITION BY id % 3 ORDER BY id) FROM customers WHERE id <= 12 ORDER BY id;
SELECT id, row_number() OVER (ORDER BY id) * 10 + rank() OVER (ORDER BY city) FROM customers WHERE id <= 8 ORDER BY 1;
SELECT ntile(0) OVER (ORDER BY id) FROM customers;
SELECT id FROM customers WHERE row_number() OVER (ORDER BY id) < 3;
SELECT upper(name) OVER () FROM customers;
