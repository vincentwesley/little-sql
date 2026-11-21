-- Shell listings and .dump across main, temp and attached files.
create table t(a);
create index ti on t(a);
create temp table tt(b);
create temp view tv as select 1;
attach 'aux1.db' as aux;
create table aux.u(c primary key);
create index aux.ui on u(c);
.tables
.tables u%
.schema
.schema u
.indexes
.databases
.dump
