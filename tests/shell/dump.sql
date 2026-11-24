-- .dump: value formatting (reals, text with CR/LF, blobs), quoting, AUTOINCREMENT, WITHOUT ROWID.
create table t(a); insert into t values('a'||char(13)||char(10)||'b'), (1.5e-7), (123456789.123), (1e17), (1.0e18+0.5), (-2.5e-300), (9e999), (-9e999), (1e16), (12345678901234567890.0), (0.000123), (100.25), (0.1), (1e300), (1.0/3), (2.0), (-0.0), (5e-324), (1.7976931348623157e308), (-123.456e-12), (4.35), (1e19), (9.3e18), (1e18), (123456789012345678.5), (x'00ff'), ('it''s'), (null), (-7);
create table "odd name"(x integer primary key autoincrement, y);
insert into "odd name"(y) values('a'),('b');
create view v as select * from t;
create index ti on t(a);
create trigger tr after insert on t begin select 1; end;
create table w(k text primary key, v) without rowid;
insert into w values('k1', 1);
.dump
.dump w
.dump odd%
