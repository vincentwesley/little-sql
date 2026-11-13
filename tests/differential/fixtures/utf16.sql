-- A UTF-16 database: text is stored as UTF-16LE on disk.
PRAGMA encoding = 'UTF-16le';
CREATE TABLE words(id INTEGER PRIMARY KEY, w TEXT COLLATE NOCASE, lang TEXT);
INSERT INTO words(w, lang) VALUES ('hello','en'),('Héllo','fr'),('こんにちは','ja'),
  ('Zebra','en'),('apple','en'),('Äpfel','de'),('😀 grin','emoji'),('',NULL);
CREATE INDEX words_w ON words(w);
