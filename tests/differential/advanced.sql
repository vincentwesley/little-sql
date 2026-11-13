-- fixture: shop
-- Features beyond the sqllogictest corpus. Known failures still count as
-- failures in the published rate.
WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 10) SELECT sum(i) FROM n;
WITH RECURSIVE fib(a, b) AS (SELECT 0, 1 UNION ALL SELECT b, a + b FROM fib WHERE a < 1000) SELECT group_concat(a) FROM fib;
WITH RECURSIVE d(day) AS (SELECT '2023-01-01' UNION ALL SELECT date(day, '+1 day') FROM d WHERE day < '2023-01-07')
SELECT d.day, count(o.id) FROM d LEFT JOIN orders o ON date(o.placed) = d.day GROUP BY d.day ORDER BY d.day;
WITH RECURSIVE chain(id, depth) AS (SELECT 1, 0 UNION SELECT c.id + 7, depth + 1 FROM chain c WHERE c.id + 7 <= 50) SELECT * FROM chain ORDER BY id;
-- known-failure: window functions are not implemented yet
SELECT id, credit, rank() OVER (ORDER BY credit DESC) FROM customers WHERE id <= 10 ORDER BY id;
-- known-failure: window functions are not implemented yet
SELECT city, id, row_number() OVER (PARTITION BY city ORDER BY id) AS rn FROM customers WHERE id <= 20 ORDER BY city, rn;
-- known-failure: window functions are not implemented yet
SELECT sku, sum(qty) OVER (ORDER BY sku ROWS BETWEEN 1 PRECEDING AND CURRENT ROW) FROM lines WHERE order_id = 7 ORDER BY sku;
-- known-failure: window functions are not implemented yet
SELECT id, lag(id) OVER (ORDER BY id), lead(id, 2, -1) OVER (ORDER BY id) FROM orders WHERE id <= 6 ORDER BY id;
-- known-failure: window functions are not implemented yet
SELECT status, count(*), sum(count(*)) OVER () FROM orders GROUP BY status ORDER BY status;
-- known-failure: JSON functions are not implemented
SELECT json_object('id', id, 'name', name) FROM customers WHERE id = 1;
SELECT iif(credit > 100, 'rich', 'poor') AS tier, count(*) FROM customers GROUP BY tier ORDER BY tier;
SELECT id FROM customers ORDER BY id DESC LIMIT 2 OFFSET 1;
SELECT CASE WHEN EXISTS (SELECT 1 FROM orders WHERE customer_id = c.id AND status = 'cancelled') THEN 'had cancel' ELSE 'clean' END, count(*) FROM customers c GROUP BY 1 ORDER BY 1;
SELECT DISTINCT o.status FROM orders o WHERE o.customer_id IN (SELECT id FROM customers WHERE city = 'Bedok') ORDER BY 1;
