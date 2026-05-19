/**
 * `@batpak/schema` — Phase 0 stub.
 *
 * This package is the **future** authoring surface for BatPAK TS events.
 * In Phase 0 it is intentionally non-authoritative: the Rust descriptor
 * registry exported via `cargo xtask export-ts-manifest` is the source
 * of truth, and `@batpak/codegen` consumes that manifest to produce the
 * generated TS in `@batpak/generated`.
 *
 * The Effect Schema v4 beta dependency is pinned exact in `package.json`
 * so the lockfile records the precise build the SDK shipped against. A
 * v4 beta bump is a deliberate, reviewed action; betas can break between
 * patch numbers.
 *
 * `bank.event()` below is the planned authoring entry point. In Phase 0
 * it is exported as a placeholder so downstream callers can pin against
 * the symbol without triggering authoring behavior. Calling it throws
 * — there is no Phase 0 use case.
 *
 * Phase 1+ will:
 *   1. Implement `bank.event()` against Effect Schema v4 declarations.
 *   2. Co-check those declarations against the Rust-emitted manifest
 *      (drift = build failure).
 * Phase 2+ will:
 *   3. Switch the authority direction: TS declarations generate Rust
 *      payload structs (with `#[derive(EventPayload)]`).
 *
 * See `bpk-lib/.phase0-audit-report.md` and the plan at
 * `/root/.claude/plans/yes-this-is-the-warm-finch.md` for context.
 */

import * as Schema from "effect/Schema";

/**
 * Phase 0 placeholder for the future authoring surface. Calling this in
 * Phase 0 throws — generated bindings in `@batpak/generated` are the
 * only authoritative TS event surface today.
 */
export const bank = {
  event<A>(_definition: unknown): Schema.Schema<A> {
    throw new BankNotAuthoritativeInPhase0(
      "bank.event() is a Phase 1+ authoring surface; use @batpak/generated for Phase 0.",
    );
  },
} as const;

/** Thrown by [`bank.event`] when invoked in Phase 0. */
export class BankNotAuthoritativeInPhase0 extends Error {
  readonly code = "bank_event_not_authoritative" as const;
  constructor(message: string) {
    super(message);
    this.name = "BankNotAuthoritativeInPhase0";
  }
}

/** Re-export the Effect Schema namespace so consumers can pin against it. */
export { Schema };
