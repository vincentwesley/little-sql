-- fixture: bigpage
-- 64 KiB pages with records large enough to overflow even there.
SELECT count(*), sum(length(body)), max(length(body)) FROM docs;
SELECT id, title, length(body), substr(body, 1, 3) FROM docs WHERE id IN (1, 100, 199, 200) ORDER BY id;
SELECT title FROM docs WHERE title > 'doc 98' ORDER BY title;
SELECT count(*) FROM docs WHERE body LIKE 'zzz%';
