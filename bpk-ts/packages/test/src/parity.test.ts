/**
 * BatPAK TS SDK 0.7.6 parity harness.
 *
 * Asserts byte-for-byte parity between the Rust-generated manifest
 * (`bpk-ts/batpak.manifest.json`) and the TypeScript canonical codec +
 * NETBAT/1 frame client across EVERY event and operation in the
 * manifest. No verb is exempt from the parity contract.
 *
 * Acceptance categories:
 *   1. Payload decode parity (every event)
 *   2. Payload encode parity (every event)
 *   3. Frame parse parity (CALL + OK + ERR) for every operation
 *   4. Frame emit parity (CALL request) for every operation
 *   5. Manifest-version guard
 *   6. ERR fixture: code + UTF-8 message decode, NOT MessagePack
 *   7. Denial-vocabulary guard (no ReceiptOutcome::Denied in ERR path)
 *   8. wireName === tsName Phase 0 invariant (every event)
 *   9. Effect 4 schema round-trip on the generated symbols
 */

import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

import { decode, decodeHex, encode, encodeHex } from "@batpak/canonical";
import {
  encodeRequest,
  parseRequestFrame,
  parseResponseFrame,
  NETBAT_ERROR_CODES,
} from "@batpak/client";
import { readManifest, type BatpakTsManifest } from "@batpak/codegen";
import { decodeBytes, encodeBytes } from "@batpak/schema";
import * as Generated from "@batpak/generated";

const here = dirname(fileURLToPath(import.meta.url));
const MANIFEST_PATH = resolve(here, "../../../batpak.manifest.json");
const manifest: BatpakTsManifest = readManifest(MANIFEST_PATH);

describe("manifest envelope", () => {
  it("declares the 0.7.6 protocol versions", () => {
    expect(manifest.manifestVersion).toBe(1);
    expect(manifest.netbatVersion).toBe("NETBAT/1");
    expect(manifest.canonicalEncoding.kind).toBe("named-field-msgpack");
    expect(manifest.canonicalEncoding.rmpSerdeVersion).toBe("1.3.1");
    expect(manifest.batpakVersion).toBe("0.7.6");
  });

  it("carries all reference hbat events", () => {
    const names = manifest.events.map((e) => e.name).sort();
    expect(names).toEqual(
      [
        "bank.commit.ack",
        "bank.commit.request",
        "event.get.ack",
        "event.get.request",
        "event.query.ack",
        "event.query.request",
        "event.query.summary",
        "system.heartbeat.ack",
        "system.heartbeat.request",
      ].sort(),
    );
  });

  it("carries all reference hbat operations", () => {
    const names = manifest.operations.map((o) => o.name).sort();
    expect(names).toEqual(["bank.commit", "event.get", "event.query", "system.heartbeat"]);
  });

  it("enforces the Phase 0 wireName === tsName invariant on every field", () => {
    for (const event of manifest.events) {
      for (const field of event.fields) {
        expect(field.wireName).toBe(field.tsName);
      }
    }
  });
});

describe("payload encode/decode parity (every event)", () => {
  for (const event of manifest.events) {
    it(`encodes ${event.name} to its goldenPayloadHex`, () => {
      const encoded = encode(event.fixtureValue);
      expect(encodeHex(encoded)).toBe(event.goldenPayloadHex);
    });

    it(`decodes ${event.name} goldenPayloadHex back to fixtureValue`, () => {
      const decoded = decode(decodeHex(event.goldenPayloadHex));
      expect(decoded).toEqual(event.fixtureValue);
    });
  }
});

describe("frame parity (every operation, every direction)", () => {
  for (const op of manifest.operations) {
    it(`encodes ${op.name} CALL frame to goldenRequestFrameHex`, () => {
      const inputBytes = decodeHex(op.goldenInputHex);
      const frame = encodeRequest(op.name, inputBytes);
      expect(encodeHex(frame)).toBe(op.goldenRequestFrameHex);
    });

    it(`parses ${op.name} goldenRequestFrameHex back to (op, payload)`, () => {
      const parsed = parseRequestFrame(decodeHex(op.goldenRequestFrameHex));
      expect(parsed.operation).toBe(op.name);
      expect(encodeHex(parsed.input)).toBe(op.goldenInputHex);
    });

    it(`parses ${op.name} goldenOkFrameHex as NetbatOk with goldenOutputHex`, () => {
      const parsed = parseResponseFrame(decodeHex(op.goldenOkFrameHex));
      expect(parsed.kind).toBe("netbat-ok");
      if (parsed.kind !== "netbat-ok") return;
      expect(encodeHex(parsed.output)).toBe(op.goldenOutputHex);
    });

    it(`parses ${op.name} errorFixture.errFrameHex as typed NetbatError(unknown_operation)`, () => {
      const parsed = parseResponseFrame(decodeHex(op.errorFixture.errFrameHex));
      expect(parsed.kind).toBe("netbat-error");
      if (parsed.kind !== "netbat-error") return;
      expect(parsed.code).toBe("unknown_operation");
      expect(parsed.message).toBe(op.errorFixture.messageUtf8);
    });
  }
});

describe("ERR fixture: message is UTF-8 text, never MessagePack", () => {
  for (const op of manifest.operations) {
    it(`${op.name}: decoding ERR.message through MessagePack must fail`, () => {
      const parsed = parseResponseFrame(decodeHex(op.errorFixture.errFrameHex));
      if (parsed.kind !== "netbat-error") {
        throw new Error("expected NetbatError");
      }
      // Try to decode the message UTF-8 bytes as MessagePack — must throw.
      expect(() => decode(new TextEncoder().encode(parsed.message))).toThrow();
    });
  }
});

describe("denial-vocabulary guard", () => {
  for (const op of manifest.operations) {
    it(`${op.name}: ERR code is a netbat token, never ReceiptOutcome::Denied`, () => {
      const parsed = parseResponseFrame(decodeHex(op.errorFixture.errFrameHex));
      if (parsed.kind !== "netbat-error") {
        throw new Error("expected NetbatError");
      }
      expect(NETBAT_ERROR_CODES).toContain(parsed.code);
      expect(parsed.message.toLowerCase()).not.toContain("denied");
      expect(parsed.message.toLowerCase()).not.toContain("denial");
    });
  }
});

describe("manifest-version guard", () => {
  it("readManifest accepts manifestVersion: 1", () => {
    expect(() => readManifest(MANIFEST_PATH)).not.toThrow();
  });

  it("readManifest refuses an unsupported manifestVersion", () => {
    const tmp = mkdtempSync(join(tmpdir(), "batpak-ts-manifest-"));
    try {
      const path = join(tmp, "batpak.manifest.json");
      const raw = readFileSync(MANIFEST_PATH, "utf-8");
      const tampered = raw.replace(`"manifestVersion": 1`, `"manifestVersion": 99`);
      writeFileSync(path, tampered, "utf-8");
      expect(() => readManifest(path)).toThrow(/manifestVersion 99 is not supported/);
    } finally {
      rmSync(tmp, { recursive: true, force: true });
    }
  });
});

describe("Effect 4 schema round-trip via @batpak/schema", () => {
  it("decodes the heartbeat request fixture through the generated schema", () => {
    const bytes = decodeHex(Generated.SYSTEM_HEARTBEAT_REQUEST_GOLDEN_HEX);
    const value = decodeBytes(Generated.SystemHeartbeatRequest, bytes);
    expect(value).toEqual(Generated.SYSTEM_HEARTBEAT_REQUEST_FIXTURE);
  });

  it("encodes the heartbeat request fixture back to the golden bytes", () => {
    const bytes = encodeBytes(
      Generated.SystemHeartbeatRequest,
      Generated.SYSTEM_HEARTBEAT_REQUEST_FIXTURE,
    );
    expect(encodeHex(bytes)).toBe(Generated.SYSTEM_HEARTBEAT_REQUEST_GOLDEN_HEX);
  });

  it("decodes the bank.commit request fixture", () => {
    const bytes = decodeHex(Generated.BANK_COMMIT_REQUEST_GOLDEN_HEX);
    const value = decodeBytes(Generated.BankCommitRequest, bytes);
    expect(value).toEqual(Generated.BANK_COMMIT_REQUEST_FIXTURE);
  });

  it("encodes the bank.commit request fixture back to the golden bytes", () => {
    const bytes = encodeBytes(Generated.BankCommitRequest, Generated.BANK_COMMIT_REQUEST_FIXTURE);
    expect(encodeHex(bytes)).toBe(Generated.BANK_COMMIT_REQUEST_GOLDEN_HEX);
  });

  it("decodes the event.get ack fixture (Option<string> + Map<string,string>)", () => {
    const bytes = decodeHex(Generated.EVENT_GET_ACK_GOLDEN_HEX);
    const value = decodeBytes(Generated.EventGetAck, bytes);
    expect(value).toEqual(Generated.EVENT_GET_ACK_FIXTURE);
  });

  it("decodes the event.query ack fixture (summary array + global sequence)", () => {
    const bytes = decodeHex(Generated.EVENT_QUERY_ACK_GOLDEN_HEX);
    const value = decodeBytes(Generated.EventQueryAck, bytes);
    expect(value).toEqual(Generated.EVENT_QUERY_ACK_FIXTURE);
    expect(value.entries[0]?.global_sequence).toBe(42);
    expect(value.entries[0]).not.toHaveProperty("payload_hex");
    expect(value.entries[0]).not.toHaveProperty("receipt_kind");
  });

  it("encodes the bank.commit ack fixture (Option + Record) back to the golden bytes", () => {
    const bytes = encodeBytes(Generated.BankCommitAck, Generated.BANK_COMMIT_ACK_FIXTURE);
    expect(encodeHex(bytes)).toBe(Generated.BANK_COMMIT_ACK_GOLDEN_HEX);
  });

  it("rejects values that fail schema constraint on encode (kind_category > 255)", () => {
    expect(() =>
      encodeBytes(Generated.BankCommitRequest, {
        entity: "x",
        scope: "y",
        kind_category: 999,
        kind_type_id: 0,
        payload_hex: "",
      }),
    ).toThrow();
  });

  it("rejects zero limits for bounded query requests", () => {
    expect(() =>
      encodeBytes(Generated.EventQueryRequest, {
        entity: null,
        scope: null,
        kind_category: null,
        kind_type_id: null,
        after_global_sequence: null,
        limit: 0,
      }),
    ).toThrow();
  });
});

describe("operation handles in generated/operations", () => {
  it("exports SYSTEM_HEARTBEAT, BANK_COMMIT, EVENT_GET, EVENT_QUERY with golden hex", () => {
    expect(Generated.SYSTEM_HEARTBEAT.name).toBe("system.heartbeat");
    expect(Generated.BANK_COMMIT.name).toBe("bank.commit");
    expect(Generated.EVENT_GET.name).toBe("event.get");
    expect(Generated.EVENT_QUERY.name).toBe("event.query");
    expect(Generated.BANK_COMMIT.errorFixture.code).toBe("unknown_operation");
  });
});
