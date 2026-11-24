#!/usr/bin/env bash
# Build the playground's demo database from data/hdb-resale-2022-onwards.csv.gz.
#
#   scripts/build-demo-db.sh [OUT]     (default: target/demo/hdb.db)
#
# Needs sqlite3. The output is deterministic for a given input.
set -euo pipefail
cd "$(dirname "$0")/.."
OUT="${1:-target/demo/hdb.db}"
mkdir -p "$(dirname "$OUT")"
rm -f "$OUT"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
gunzip -c data/hdb-resale-2022-onwards.csv.gz > "$TMP/hdb.csv"

sqlite3 "$OUT" <<SQL
PRAGMA page_size = 4096;
CREATE TABLE raw(month, town, flat_type, block, street_name, storey_range,
  floor_area_sqm, flat_model, lease_commence_date, remaining_lease, resale_price);
.import --csv --skip 1 $TMP/hdb.csv raw

CREATE TABLE towns(
  id INTEGER PRIMARY KEY,
  name TEXT NOT NULL UNIQUE,
  region TEXT NOT NULL
);
INSERT INTO towns(name, region)
SELECT name, CASE
    WHEN name IN ('BISHAN','BUKIT MERAH','BUKIT TIMAH','CENTRAL AREA','GEYLANG',
                  'KALLANG/WHAMPOA','MARINE PARADE','QUEENSTOWN','TOA PAYOH') THEN 'Central'
    WHEN name IN ('BEDOK','PASIR RIS','TAMPINES') THEN 'East'
    WHEN name IN ('SEMBAWANG','WOODLANDS','YISHUN') THEN 'North'
    WHEN name IN ('ANG MO KIO','HOUGANG','PUNGGOL','SENGKANG','SERANGOON') THEN 'North-East'
    ELSE 'West'
  END
FROM (SELECT DISTINCT town AS name FROM raw ORDER BY town);

CREATE TABLE resale(
  id INTEGER PRIMARY KEY,
  month TEXT NOT NULL,              -- YYYY-MM the resale was registered
  town_id INTEGER NOT NULL REFERENCES towns(id),
  flat_type TEXT NOT NULL,          -- '1 ROOM' ... 'EXECUTIVE'
  block TEXT NOT NULL,
  street TEXT NOT NULL,
  storey_range TEXT NOT NULL,       -- e.g. '10 TO 12'
  floor_area_sqm REAL NOT NULL,
  flat_model TEXT NOT NULL,
  lease_commence_year INTEGER NOT NULL,
  remaining_lease TEXT NOT NULL,    -- e.g. '61 years 04 months'
  price INTEGER NOT NULL            -- Singapore dollars
);
INSERT INTO resale(month, town_id, flat_type, block, street, storey_range,
  floor_area_sqm, flat_model, lease_commence_year, remaining_lease, price)
SELECT r.month, t.id, r.flat_type, r.block, r.street_name, r.storey_range,
  CAST(r.floor_area_sqm AS REAL), r.flat_model, CAST(r.lease_commence_date AS INTEGER),
  r.remaining_lease, CAST(r.resale_price AS INTEGER)
FROM raw r JOIN towns t ON t.name = r.town
ORDER BY r.month, t.id, r.rowid;
DROP TABLE raw;

CREATE INDEX resale_town_month ON resale(town_id, month);
CREATE INDEX resale_month ON resale(month);
CREATE INDEX resale_type_price ON resale(flat_type, price);

CREATE VIEW sales AS
SELECT r.id, r.month, t.name AS town, t.region, r.flat_type, r.block, r.street,
  r.storey_range, r.floor_area_sqm, r.flat_model, r.lease_commence_year,
  r.price, round(r.price / r.floor_area_sqm) AS price_per_sqm
FROM resale r JOIN towns t ON t.id = r.town_id;

VACUUM;
SQL
echo "built $OUT: $(sqlite3 "$OUT" 'SELECT count(*) FROM resale') sales, $(stat -c %s "$OUT") bytes"
