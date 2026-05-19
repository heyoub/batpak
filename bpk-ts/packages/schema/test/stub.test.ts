import { describe, expect, it } from "vitest";

import { bank, BankNotAuthoritativeInPhase0 } from "../src/index.js";

describe("Phase 0 bank.event() stub", () => {
  it("throws BankNotAuthoritativeInPhase0 with a stable code", () => {
    try {
      bank.event({ name: "test.event" });
      throw new Error("expected bank.event() to throw");
    } catch (error) {
      expect(error).toBeInstanceOf(BankNotAuthoritativeInPhase0);
      if (error instanceof BankNotAuthoritativeInPhase0) {
        expect(error.code).toBe("bank_event_not_authoritative");
      }
    }
  });

  it("does NOT promote @batpak/schema to authoritative source in Phase 0", () => {
    // This test exists so a future maintainer who removes the throw
    // sees it fail and is forced to ask: am I moving authority into
    // TS? If yes, that's a Phase 1+ change that needs review.
    expect(() => bank.event({ name: "anything" })).toThrow();
  });
});
