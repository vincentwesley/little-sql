# Data

`hdb-resale-2022-onwards.csv.gz` holds every HDB resale flat transaction
registered from January 2022 to November 2026 (127,863 rows; November is partial), taken unchanged
from the data.gov.sg dataset
[Resale flat prices based on registration date from Jan-2017 onwards](https://data.gov.sg/datasets/d_8b84c4ee58e3cfc0ece0d773c8ca6abc/view),
downloaded on 12 November 2026, keeping only rows whose month is 2022-01 or
later.

> Contains information from Resale flat prices based on registration date
> from Jan-2017 onwards, accessed on 12 November 2026 from data.gov.sg, which
> is made available under the terms of the
> [Singapore Open Data Licence version 1.0](https://data.gov.sg/open-data-licence).

`scripts/build-demo-db.sh` turns it into the playground's demo database.
