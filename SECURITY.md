# Security Policy

## Reporting a vulnerability

Report security issues **privately** — not in public issues or pull requests.

Use GitHub's private vulnerability reporting:
<https://github.com/heyoub/batpak/security/advisories/new>

You'll get an acknowledgement and an assessment or fix as maintainer time
allows. batpak is maintainer-led, so there's no formal SLA — but security
reports are prioritized over feature work.

## Supported versions

batpak is pre-1.0 (`0.x`). Only the latest published release line receives
fixes. Pin a version you've verified; `0.x` minor bumps may carry breaking
changes (the minor bump *is* the breaking signal).

## Scope

In scope:

- Memory-safety issues.
- Data-integrity violations — corruption, lost writes, idempotency or
  visibility/durability guarantees not holding.
- Anything that lets untrusted input compromise a store.

Out of scope:

- Issues requiring an already-compromised host.
- Deliberate misuse of APIs documented as `dangerous_*` / unsafe.
