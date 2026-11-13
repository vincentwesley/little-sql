# Security policy

Little SQL is a learning and portfolio project. It is not hardened for
untrusted input in production, but bugs that let a crafted database file or
query crash the process, hang it, or read or write outside the database are
taken seriously.

## Reporting a vulnerability

Please report privately through GitHub's
[private vulnerability reporting](https://docs.github.com/en/code-security/security-advisories/guidance-on-reporting-and-writing-information-about-vulnerabilities/privately-reporting-a-security-vulnerability)
on this repository (Security tab → "Report a vulnerability"). Do not open a
public issue for a security problem.

Include the Little SQL commit, the database file or SQL that triggers the
problem (or a script that builds it), and what happened. You should get an
acknowledgement within a week.

## Scope

- The engine (`crates/littlesql`) opening a malformed or hostile SQLite
  file: it should return an error, never panic, loop forever or allocate
  without bound.
- The playground: it runs entirely in the visitor's browser; the database a
  visitor opens never leaves their machine.

Out of scope: concurrent access from several processes (Little SQL takes no
file locks, by design; see docs/DECISIONS.md D13).

## Supported versions

Only the latest commit on the default branch.
