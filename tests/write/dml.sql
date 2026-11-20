-- Changes applied by both engines to a database sqlite3 created
-- (scripts/write-check.sh), so Little SQL edits a file it did not write.
INSERT INTO customers(name, email, city, joined, credit) VALUES ('New One', 'new@example.com', 'Tengah', '2026-11-01', 12.5);
UPDATE customers SET credit = credit * 2 WHERE city = 'Bedok';
DELETE FROM orders WHERE status = 'cancelled';
UPDATE products SET price = price + 1, title = title || ' (new)' WHERE category = 'veg';
DELETE FROM lines WHERE qty > 7;
INSERT INTO lines SELECT order_id + 5000, sku, qty FROM lines WHERE order_id < 100;
CREATE INDEX orders_status ON orders(status);
DROP INDEX customers_city;
