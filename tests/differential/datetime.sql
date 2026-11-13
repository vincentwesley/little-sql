-- fixture: shop
-- Date and time functions on fixed inputs (never 'now').
SELECT date('2024-02-29'), date('2024-02-29', '+1 year'), date('2023-01-31', '+1 month'), date('2024-03-31', '-1 month');
SELECT time('12:34:56'), datetime('2024-01-01 23:59:59', '+1 second'), datetime(0, 'unixepoch'), datetime(1700000000, 'unixepoch');
SELECT julianday('2000-01-01'), julianday('2000-01-01 12:00'), unixepoch('2024-01-01'), strftime('%s', '2024-01-01');
SELECT strftime('%Y-%m-%d %H:%M:%S', '2024-07-04 09:08:07'), strftime('%j %w %W %U', '2024-12-31'), strftime('%f', '2024-01-01 00:00:01.250');
SELECT date('2024-01-15', 'start of month'), date('2024-05-15', 'start of year'), date('2024-01-15', 'weekday 0'), datetime('2024-01-15 10:00', 'start of day');
SELECT date('2024-01-01', '+10 days', '-2 months'), datetime('2024-01-01', '+36 hours'), datetime('2024-01-01', '+90 minutes'), date('2024-01-01', '-0001-00-00');
SELECT date('not a date'), date(NULL), date('2024-13-01'), datetime('2024-01-01T10:20:30Z'), datetime('2024-01-01 10:20:30+02:00');
SELECT date(2460000.5), datetime(2460000.25), time(0.5), date('2024-01-01', 'unixepoch');
SELECT substr(placed, 1, 10), count(*) FROM orders WHERE placed < '2023-01-03' GROUP BY 1 ORDER BY 1;
SELECT id, strftime('%Y-%W', placed) FROM orders WHERE id <= 5 ORDER BY id;
SELECT count(*) FROM orders WHERE date(placed) BETWEEN '2023-03-01' AND '2023-03-31';
SELECT id, julianday(placed) - julianday('2023-01-01') FROM orders WHERE id IN (1, 2, 3) ORDER BY id;
SELECT id, joined, date(joined, '+1 month', 'start of month') FROM customers WHERE id <= 4 ORDER BY id;
