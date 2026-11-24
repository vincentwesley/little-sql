# Roadmap to 1.0 (the finish line)

Little SQL is feature-complete for what it set out to be: a from-scratch,
dependency-free SQLite-compatible engine, shell and browser playground,
graded against SQLite's own tests. From here the only goal is a polished,
shippable 1.0. **No new features.**

## Definition of done

The project is finished when every box is ticked:

- [x] Feature freeze in force (see "Out of scope" below).
- [x] README reads well for a stranger: what it is, a screenshot, a live
      demo link, how to install, the scores, and an honest list of limits.
- [x] A release workflow builds the shell for Linux, macOS and Windows and
      the playground as a zip, attached to a GitHub release on a `v*` tag.
- [x] Docs tidied: a final status in HANDOVER, a CHANGELOG with the 1.0
      notes, and NEEDS-VIN reduced to the owner's remaining clicks.
- [ ] Acceptance passes from a fresh clone (all 20 checks) and CI is green.
- [ ] Owner steps done (only the repository owner can do these):
  - [ ] Enable GitHub Pages (Settings → Pages → Source: GitHub Actions)
        and run the Pages workflow, which gives the live demo URL.
  - [ ] Merge pull request #1 into `main`.
  - [ ] Push the tag `v1.0.0` on `main`, which publishes the release.
  - [ ] Optional: set the repository description and topics, and pin it.

## Out of scope (frozen)

These are known and documented, and deliberately not pursued:

- SQLite's cost model for joins (joins use simpler heuristics; the answers
  are the same, only some plans differ).
- `jsonb_*` functions, other virtual tables, foreign key enforcement.
- min/max index seeks, SQLite's WHERE constant rewriting.
- Multi-process file locking, and SQLite-level speed (a bytecode VM).

Each is listed in the README's limits and in HANDOVER.

## After 1.0

Maintenance only: fix a reported bug if one comes in. Anything else would
start a new, separately scoped project.
