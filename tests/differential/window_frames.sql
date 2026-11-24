-- fixture: shop
-- Window frames: RANGE with numeric offsets, and EXCLUDE.
WITH t(a,b) AS (VALUES (1,1),(2,1),(4,2),(4,3),(7,NULL),(NULL,5),(NULL,6),('x',7),(2.5,8)) SELECT a, sum(b) over (order by a range between 1 preceding and 1 following) FROM t ORDER BY 1, 2, 3;
WITH t(a,b) AS (VALUES (1,1),(2,1),(4,2),(4,3),(7,NULL),(NULL,5),(NULL,6),('x',7),(2.5,8)) SELECT a, sum(b) over (order by a, b range 1 preceding) FROM t ORDER BY 1, 2, 3;
WITH t(a,b) AS (VALUES (1,1),(2,1),(4,2),(4,3),(7,NULL),(NULL,5),(NULL,6),('x',7),(2.5,8)) SELECT a, sum(b) over (range 1 preceding) FROM t ORDER BY 1, 2, 3;
WITH t(a,b) AS (VALUES (1,1),(2,1),(4,2),(4,3),(7,NULL),(NULL,5),(NULL,6),('x',7),(2.5,8)) SELECT a, sum(b) over (order by a range -1 preceding) FROM t ORDER BY 1, 2, 3;
WITH t(a,b) AS (VALUES (1,1),(2,1),(4,2),(4,3),(7,NULL),(NULL,5),(NULL,6),('x',7),(2.5,8)) SELECT a, sum(b) over (order by a range 'x' preceding) FROM t ORDER BY 1, 2, 3;
WITH t(a,b) AS (VALUES (1,1),(2,1),(4,2),(4,3),(7,NULL),(NULL,5),(NULL,6),('x',7),(2.5,8)) SELECT a, sum(b) over (order by a rows between 1 preceding and -1 following) FROM t ORDER BY 1, 2, 3;
WITH t(a,b) AS (VALUES (1,1),(2,1),(4,2),(4,3),(7,NULL),(NULL,5),(NULL,6),('x',7),(2.5,8)) SELECT a, sum(b) over (order by a range between 1 preceding and 'q' following) FROM t ORDER BY 1, 2, 3;
WITH t(a,b) AS (VALUES (1,1),(2,1),(4,2),(4,3),(7,NULL),(NULL,5),(NULL,6),('x',7),(2.5,8)) SELECT a, sum(b) over (order by a range 1.5 preceding) FROM t ORDER BY 1, 2, 3;
WITH t(a,b) AS (VALUES (1,1),(2,1),(4,2),(4,3),(7,NULL),(NULL,5),(NULL,6),('x',7),(2.5,8)) SELECT a, sum(b) over (order by a rows 1.5 preceding) FROM t ORDER BY 1, 2, 3;
WITH t(a,b) AS (VALUES (1,1),(2,1),(4,2),(4,3),(7,NULL),(NULL,5),(NULL,6),('x',7),(2.5,8)) SELECT a, sum(b) over (order by a desc range between 2 preceding and current row) FROM t ORDER BY 1, 2, 3;
WITH t(a,b) AS (VALUES (1,1),(2,1),(4,2),(4,3),(7,NULL),(NULL,5),(NULL,6),('x',7),(2.5,8)) SELECT a, group_concat(b) over (order by a rows between unbounded preceding and unbounded following exclude current row) FROM t ORDER BY 1, 2, 3;
WITH t(a,b) AS (VALUES (1,1),(2,1),(4,2),(4,3),(7,NULL),(NULL,5),(NULL,6),('x',7),(2.5,8)) SELECT a, group_concat(b) over (order by a groups between 1 preceding and 1 following exclude group) FROM t ORDER BY 1, 2, 3;
WITH t(a,b) AS (VALUES (1,1),(2,1),(4,2),(4,3),(7,NULL),(NULL,5),(NULL,6),('x',7),(2.5,8)) SELECT a, group_concat(b) over (order by a range between unbounded preceding and current row exclude ties) FROM t ORDER BY 1, 2, 3;
WITH t(a,b) AS (VALUES (1,1),(2,1),(4,2),(4,3),(7,NULL),(NULL,5),(NULL,6),('x',7),(2.5,8)) SELECT a, first_value(b) over (order by a rows between current row and 2 following exclude current row), last_value(b) over (order by a rows 2 preceding exclude ties), nth_value(b,2) over (order by a range between unbounded preceding and unbounded following exclude group) FROM t ORDER BY 1, 2, 3;
WITH t(a,b) AS (VALUES (1,1),(2,1),(4,2),(4,3),(7,NULL),(NULL,5),(NULL,6),('x',7),(2.5,8)) SELECT a, count(*) over (order by a desc nulls first range between 1.5 following and 3 following), sum(b) over (order by a nulls last range between current row and 2.5 following exclude no others) FROM t ORDER BY 1, 2, 3;
WITH t(a,b) AS (VALUES (1,1),(2,1),(4,2),(4,3),(7,NULL),(NULL,5),(NULL,6),('x',7),(2.5,8)) SELECT b, sum(a) over (partition by b > 3 order by b range between 2 preceding and 1 preceding), avg(b) over (order by b range between 9223372036854775807 preceding and 9223372036854775807 following) FROM t ORDER BY 1, 2, 3;
WITH t(a,b) AS (VALUES (1,1),(2,1),(4,2),(4,3),(7,NULL),(NULL,5),(NULL,6),('x',7),(2.5,8)) SELECT a, sum(b) over (order by a range between 2 following and 1 following) FROM t ORDER BY 1, 2, 3;
WITH t(a,b) AS (VALUES (1,1),(2,1),(4,2),(4,3),(7,NULL),(NULL,5),(NULL,6),('x',7),(2.5,8)) SELECT a, sum(b) over w FROM t WINDOW w AS (ORDER BY a RANGE 1 PRECEDING EXCLUDE CURRENT ROW) ORDER BY 1, 2;
SELECT customer_id, total, sum(total) OVER (PARTITION BY customer_id ORDER BY total RANGE BETWEEN 50 PRECEDING AND 50 FOLLOWING EXCLUDE CURRENT ROW) FROM order_totals WHERE customer_id <= 6 ORDER BY 1, 2, 3;
