-- fixture: shop
-- Inner, outer, cross and self joins, USING/NATURAL, join order.
SELECT o.id, c.name FROM orders o JOIN customers c ON c.id = o.customer_id WHERE o.id < 15 ORDER BY o.id;
SELECT o.id, c.name FROM orders o LEFT JOIN customers c ON c.id = o.customer_id WHERE o.id % 50 = 0 ORDER BY o.id LIMIT 5;
SELECT c.id, count(o.id) FROM customers c LEFT JOIN orders o ON o.customer_id = c.id GROUP BY c.id HAVING count(o.id) = 0;
SELECT c.city, p.category, sum(l.qty) FROM customers c JOIN orders o ON o.customer_id = c.id JOIN lines l ON l.order_id = o.id JOIN products p ON p.sku = l.sku GROUP BY 1, 2 ORDER BY 1, 2;
SELECT l.order_id, p.title FROM lines l, products p WHERE p.sku = l.sku AND l.order_id = 1000 ORDER BY 2;
SELECT a.id, b.id FROM customers a JOIN customers b ON a.email = b.email AND a.id < b.id;
SELECT a.id, b.id FROM customers a JOIN customers b ON b.id = a.id + 1 WHERE a.city = b.city ORDER BY a.id LIMIT 10;
SELECT count(*) FROM products a CROSS JOIN products b WHERE a.price < b.price;
SELECT p.sku, l.qty FROM products p LEFT JOIN lines l ON l.sku = p.sku AND l.order_id = 5 ORDER BY p.sku LIMIT 10;
SELECT p.sku FROM products p LEFT JOIN lines l ON l.sku = p.sku WHERE l.sku IS NULL;
SELECT order_id, sku, title FROM lines JOIN products USING (sku) WHERE order_id = 77 ORDER BY sku;
SELECT * FROM lines NATURAL JOIN products WHERE order_id = 12 ORDER BY sku;
SELECT c.id, o.id, l.sku FROM customers c LEFT JOIN orders o ON o.customer_id = c.id LEFT JOIN lines l ON l.order_id = o.id WHERE c.id = 3 ORDER BY 2, 3;
SELECT c.name, t.total FROM customers c JOIN order_totals t ON t.customer_id = c.id WHERE c.id = 10 ORDER BY t.order_id;
SELECT x.city, y.n FROM (SELECT DISTINCT city FROM customers) x JOIN (SELECT city, count(*) n FROM customers GROUP BY city) y ON x.city = y.city ORDER BY 1;
SELECT o.id FROM orders o JOIN customers c ON c.id = o.customer_id AND c.city = 'Punggol' WHERE o.status = 'pending' ORDER BY o.id LIMIT 8;
SELECT count(*) FROM orders o, lines l WHERE o.id = l.order_id AND o.status = 'shipped';
SELECT c.id FROM customers c WHERE c.id < 5 AND EXISTS (SELECT 1 FROM orders o WHERE o.customer_id = c.id) ORDER BY 1;
SELECT p.title, p2.title FROM products p JOIN products p2 ON p.title = p2.title COLLATE BINARY AND p.sku <> p2.sku;
SELECT a.sku, b.sku FROM products a RIGHT JOIN products b ON a.weight = b.sku WHERE b.price < 3;
