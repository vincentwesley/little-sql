# Security policy

## Reporting a vulnerability
Please report security issues privately through GitHub: **Security tab, "Report a vulnerability"** on this repository.
Do not open a public issue for a vulnerability. You can expect an acknowledgement within a few days.

## Scope and safety notes
- This project is provided "as is", without warranty (see `LICENSE`).
- It contains no install-time scripts (`preinstall`/`postinstall`/`prepare`) and does not run code on your machine when you clone it.
- Secrets are never committed. Configuration is read from environment variables; see `.env.example` where present.
- Only the latest version on the default branch is supported.

Little SQL is a learning and portfolio project. It is not hardened for
untrusted input in production, but bugs that let a crafted database file or
query crash the process, hang it, or read or write outside the database are
taken seriously. When reporting, include the Little SQL commit, the database
file or SQL that triggers the problem (or a script that builds it), and what
happened.

## What counts for Little SQL

- The engine (`crates/littlesql`) opening a malformed or hostile SQLite
  file: it should return an error, never panic, loop forever or allocate
  without bound.
- The playground: it runs entirely in the visitor's browser; the database a
  visitor opens never leaves their machine.

Out of scope: concurrent access from several processes (Little SQL takes no
file locks, by design; see docs/DECISIONS.md D13).

