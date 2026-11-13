-- fixture: utf16
-- A UTF-16LE database: text decoding, NOCASE index, lengths.
SELECT * FROM words ORDER BY id;
SELECT w FROM words ORDER BY w;
SELECT w, length(w), hex(w), upper(w) FROM words ORDER BY id;
SELECT id FROM words WHERE w = 'HELLO';
SELECT lang, count(*) FROM words GROUP BY lang ORDER BY lang;
SELECT w FROM words WHERE w > 'h' ORDER BY w;
PRAGMA encoding;
