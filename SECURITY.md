# Security policy

## Reporting a vulnerability
Please report security issues privately through GitHub: **Security tab, "Report a vulnerability"** on this repository.
Do not open a public issue for a vulnerability. You can expect an acknowledgement within a few days.

## Scope and safety notes
- This project is provided "as is", without warranty (see `LICENSE`).
- It contains no install-time scripts (`preinstall`/`postinstall`/`prepare`) and does not run code on your machine when you clone it.
- Secrets are never committed. Configuration is read from environment variables; see `.env.example` where present.
- Only the latest version on the default branch is supported.
