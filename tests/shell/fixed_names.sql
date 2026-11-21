-- Names inside non-TEMP views and triggers resolve in the object's own
-- file, not by the usual temp-first search.
create table t(a);
insert into t values('main');
create view v as select a from t;
create temp table t(a);
insert into temp.t values('temp');
select * from v;
select * from t;
attach 'aux1.db' as aux;
create table aux.t(a);
insert into aux.t values('aux');
create view aux.av as select a from t;
select * from aux.av;
create table aux.log(m);
create trigger aux.tr after insert on aux.t begin insert into log select a from t; end;
insert into aux.t values('x');
select * from aux.log;
create temp view tv as select a from t;
select * from tv;
create view main.v2 as select a from temp.t;
select * from (select * from v) join aux.av order by 2;
create temp trigger ttr after insert on main.t begin insert into temp.t values('fired ' || new.a); end;
create temp trigger ttr after insert on main.t begin insert into t values('fired ' || new.a); end;
insert into main.t values('m2');
select * from temp.t;
select * from main.t;
drop table main.t;
select name from sqlite_temp_master;
