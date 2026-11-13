-- A small shop: customers, products, orders and order lines, with NULLs,
-- mixed types, indexes of several shapes and a view. Built by sqlite3.
CREATE TABLE customers(
  id INTEGER PRIMARY KEY,
  name TEXT NOT NULL,
  email TEXT UNIQUE,
  city TEXT,
  joined TEXT,
  credit REAL
);
CREATE TABLE products(
  sku TEXT PRIMARY KEY,
  title TEXT COLLATE NOCASE,
  price NUMERIC,
  category TEXT,
  weight
);
CREATE TABLE orders(
  id INTEGER PRIMARY KEY,
  customer_id INTEGER REFERENCES customers(id),
  placed TEXT,
  status TEXT
);
CREATE TABLE lines(
  order_id INTEGER,
  sku TEXT,
  qty INTEGER,
  PRIMARY KEY(order_id, sku)
) WITHOUT ROWID;
CREATE INDEX orders_customer ON orders(customer_id, placed DESC);
CREATE INDEX customers_city ON customers(city);
CREATE INDEX products_cat_price ON products(category, price);
CREATE INDEX lines_sku ON lines(sku);

WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i < 300)
INSERT INTO customers
SELECT i,
       'Customer ' || i,
       CASE WHEN i % 17 = 0 THEN NULL ELSE 'c' || i || '@example.com' END,
       CASE i % 7 WHEN 0 THEN 'Ang Mo Kio' WHEN 1 THEN 'Bedok' WHEN 2 THEN 'Clementi'
                  WHEN 3 THEN 'Jurong West' WHEN 4 THEN 'Punggol' WHEN 5 THEN NULL
                  ELSE 'Queenstown' END,
       date('2020-01-01', '+' || (i * 3) || ' days'),
       CASE WHEN i % 11 = 0 THEN NULL ELSE round((i * 37 % 1000) / 7.0, 2) END
FROM n;

WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i < 60)
INSERT INTO products
SELECT printf('SKU-%03d', i),
       CASE i % 4 WHEN 0 THEN 'apple ' WHEN 1 THEN 'Banana ' WHEN 2 THEN 'cherry ' ELSE 'Durian ' END || i,
       CASE WHEN i % 5 = 0 THEN i * 2 ELSE i * 1.25 END,
       CASE i % 3 WHEN 0 THEN 'fruit' WHEN 1 THEN 'veg' ELSE NULL END,
       CASE i % 4 WHEN 0 THEN i WHEN 1 THEN i / 10.0 WHEN 2 THEN 'heavy' ELSE NULL END
FROM n;

WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i < 2000)
INSERT INTO orders
SELECT i,
       CASE WHEN i % 50 = 0 THEN NULL ELSE (i * 7919) % 300 + 1 END,
       datetime('2023-01-01 08:00:00', '+' || (i * 97 % 525600) || ' minutes'),
       CASE i % 5 WHEN 0 THEN 'cancelled' WHEN 1 THEN 'pending' ELSE 'shipped' END
FROM n;

WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i < 6000)
INSERT OR IGNORE INTO lines
SELECT (i * 13) % 2000 + 1, printf('SKU-%03d', (i * 31) % 60 + 1), i % 9 + 1 FROM n;

CREATE VIEW order_totals AS
SELECT o.id AS order_id, o.customer_id, sum(l.qty * p.price) AS total, count(*) AS n_lines
FROM orders o JOIN lines l ON l.order_id = o.id JOIN products p ON p.sku = l.sku
GROUP BY o.id;
