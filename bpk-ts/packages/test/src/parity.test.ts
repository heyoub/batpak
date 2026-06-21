/**
 * BatPAK TS SDK 0.8.3 parity harness.
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
  isCompatiblePayloadVersion,
  classifyPayloadVersion,
} from "@batpak/client";
import { readManifest, type BatpakTsManifest } from "@batpak/codegen";
import { decodeBytes, encodeBytes } from "@batpak/schema";
import * as Generated from "@batpak/generated";

const here = dirname(fileURLToPath(import.meta.url));
const MANIFEST_PATH = resolve(here, "../../../batpak.manifest.json");
const manifest: BatpakTsManifest = readManifest(MANIFEST_PATH);

describe("manifest envelope", () => {
  it("declares the 0.8.3 protocol versions", () => {
    // manifestVersion 2: schema-evolution Phase 2 made each event's
    // payload_version manifest-visible (see payloadVersion below).
    expect(manifest.manifestVersion).toBe(2);
    expect(manifest.netbatVersion).toBe("NETBAT/1");
    expect(manifest.canonicalEncoding.kind).toBe("named-field-msgpack");
    expect(manifest.canonicalEncoding.rmpSerdeVersion).toBe("1.3.1");
    expect(manifest.batpakVersion).toBe("0.8.3");
  });

  it("carries a declared payloadVersion (>= 1) on every event", () => {
    for (const event of manifest.events) {
      expect(Number.isInteger(event.payloadVersion)).toBe(true);
      // 0 is the legacy/untyped sentinel and is never a DECLARED version.
      expect(event.payloadVersion).toBeGreaterThanOrEqual(1);
    }
  });

  it("carries all reference refbat events", () => {
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
        "event.walk.ack",
        "event.walk.request",
        "evidence.chain_walk.ack",
        "evidence.chain_walk.request",
        "evidence.projection_run.ack",
        "evidence.projection_run.request",
        "evidence.read_walk.ack",
        "evidence.read_walk.request",
        "evidence.store_resource.ack",
        "evidence.store_resource.request",
        "receipt.verify.ack",
        "receipt.verify.request",
        "system.heartbeat.ack",
        "system.heartbeat.request",
      ].sort(),
    );
  });

  it("carries all reference refbat operations", () => {
    const names = manifest.operations.map((o) => o.name).sort();
    expect(names).toEqual([
      "bank.commit",
      "event.get",
      "event.query",
      "event.walk",
      "evidence.chain_walk",
      "evidence.projection_run",
      "evidence.read_walk",
      "evidence.store_resource",
      "receipt.verify",
      "system.heartbeat",
    ]);
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
  it("readManifest accepts manifestVersion: 2", () => {
    expect(() => readManifest(MANIFEST_PATH)).not.toThrow();
  });

  it("readManifest refuses an unsupported manifestVersion", () => {
    const tmp = mkdtempSync(join(tmpdir(), "batpak-ts-manifest-"));
    try {
      const path = join(tmp, "batpak.manifest.json");
      const raw = readFileSync(MANIFEST_PATH, "utf-8");
      const tampered = raw.replace(`"manifestVersion": 2`, `"manifestVersion": 99`);
      writeFileSync(path, tampered, "utf-8");
      expect(() => readManifest(path)).toThrow(/manifestVersion 99 is not supported/);
    } finally {
      rmSync(tmp, { recursive: true, force: true });
    }
  });

  it("readManifest refuses the now-obsolete manifestVersion 1", () => {
    const tmp = mkdtempSync(join(tmpdir(), "batpak-ts-manifest-"));
    try {
      const path = join(tmp, "batpak.manifest.json");
      const raw = readFileSync(MANIFEST_PATH, "utf-8");
      const tampered = raw.replace(`"manifestVersion": 2`, `"manifestVersion": 1`);
      writeFileSync(path, tampered, "utf-8");
      expect(() => readManifest(path)).toThrow(/manifestVersion 1 is not supported/);
    } finally {
      rmSync(tmp, { recursive: true, force: true });
    }
  });
});

describe("forward-compat payload_version decode", () => {
  it("tolerates a stored payload_version newer than the generated SDK", () => {
    // The server upcasts older shapes on read and additive evolution is
    // ignored by the generated Schema, so a NEWER stored version still
    // decodes against the current shape — never hard-reject.
    const generated = Generated.SYSTEM_HEARTBEAT_REQUEST_PAYLOAD_VERSION;
    expect(isCompatiblePayloadVersion(generated + 1, generated)).toBe(true);
    expect(classifyPayloadVersion(generated + 1, generated)).toBe("newer");
  });

  it("tolerates the legacy/untyped sentinel (0) and exact/older versions", () => {
    const generated = Generated.SYSTEM_HEARTBEAT_REQUEST_PAYLOAD_VERSION;
    expect(isCompatiblePayloadVersion(0, generated)).toBe(true);
    expect(classifyPayloadVersion(0, generated)).toBe("legacy");
    expect(isCompatiblePayloadVersion(generated, generated)).toBe(true);
    expect(classifyPayloadVersion(generated, generated)).toBe("exact");
  });

  it("decodes the heartbeat fixture regardless of the wire payload_version", () => {
    // Forward-compat contract: the payload BYTES decode the same; the
    // version tag rides in the header, not the payload struct, so a newer
    // tag does not perturb payload decode.
    const bytes = decodeHex(Generated.SYSTEM_HEARTBEAT_REQUEST_GOLDEN_HEX);
    const value = decodeBytes(Generated.SystemHeartbeatRequest, bytes);
    expect(value).toEqual(Generated.SYSTEM_HEARTBEAT_REQUEST_FIXTURE);
    expect(isCompatiblePayloadVersion(99, Generated.SYSTEM_HEARTBEAT_REQUEST_PAYLOAD_VERSION)).toBe(
      true,
    );
  });

  it("rejects only a malformed (non-integer / negative) stored version", () => {
    const generated = Generated.SYSTEM_HEARTBEAT_REQUEST_PAYLOAD_VERSION;
    expect(isCompatiblePayloadVersion(-1, generated)).toBe(false);
    expect(isCompatiblePayloadVersion(1.5, generated)).toBe(false);
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

  it("encodes bank.commit with idempotency_key_hex OMITTED to the same golden bytes (#28)", () => {
    // `idempotency_key_hex` is backed by a Rust `#[serde(default)] Option<String>`,
    // so the TS property is omittable. Omitting the key MUST still encode as
    // present-nil — byte-identical to the `idempotency_key_hex: null` fixture
    // and to Rust's `to_vec_named` None → nil. This is the load-bearing
    // proof that making the field omittable did NOT change the wire.
    const omitted = encodeBytes(Generated.BankCommitRequest, {
      entity: "fixture:bank",
      scope: "fixture-scope",
      kind_category: 15,
      kind_type_id: 2561,
      payload_hex: "81a56e6f6e6365b66865617274626561742d666978747572652d30303031",
      // idempotency_key_hex intentionally OMITTED
    } as unknown as typeof Generated.BankCommitRequest.Type);
    expect(encodeHex(omitted)).toBe(Generated.BANK_COMMIT_REQUEST_GOLDEN_HEX);
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

  it("decodes the receipt.verify ack fixture (valid/outcome/reason_code)", () => {
    const bytes = decodeHex(Generated.RECEIPT_VERIFY_ACK_GOLDEN_HEX);
    const value = decodeBytes(Generated.ReceiptVerifyAck, bytes);
    expect(value).toEqual(Generated.RECEIPT_VERIFY_ACK_FIXTURE);
    expect(value.valid).toBe(true);
    expect(value.outcome).toBe("unsigned_accepted");
  });

  it("decodes the event.walk ack fixture (summary array in relation order)", () => {
    const bytes = decodeHex(Generated.EVENT_WALK_ACK_GOLDEN_HEX);
    const value = decodeBytes(Generated.EventWalkAck, bytes);
    expect(value).toEqual(Generated.EVENT_WALK_ACK_FIXTURE);
    expect(value.entries[0]?.event_id_hex).toHaveLength(32);
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

  it("rejects zero limits for bounded traversal requests", () => {
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

    expect(() =>
      encodeBytes(Generated.EventWalkRequest, {
        event_id_hex: "00000000000000000000000000000000",
        limit: 0,
      }),
    ).toThrow();

    expect(() =>
      encodeBytes(Generated.ReadWalkEvidenceRequest, {
        entity: null,
        scope: null,
        kind_category: null,
        kind_type_id: null,
        start_clock: null,
        end_clock: null,
        limit: 0,
        include_proof_refs: false,
        max_stale_ms: null,
      }),
    ).toThrow();
  });
});

describe("operation handles in generated/operations", () => {
  it("exports all ten reference operations with golden hex", () => {
    expect(Generated.SYSTEM_HEARTBEAT.name).toBe("system.heartbeat");
    expect(Generated.BANK_COMMIT.name).toBe("bank.commit");
    expect(Generated.EVENT_GET.name).toBe("event.get");
    expect(Generated.EVENT_QUERY.name).toBe("event.query");
    expect(Generated.RECEIPT_VERIFY.name).toBe("receipt.verify");
    expect(Generated.EVENT_WALK.name).toBe("event.walk");
    expect(Generated.EVIDENCE_CHAIN_WALK.name).toBe("evidence.chain_walk");
    expect(Generated.EVIDENCE_STORE_RESOURCE.name).toBe("evidence.store_resource");
    expect(Generated.EVIDENCE_READ_WALK.name).toBe("evidence.read_walk");
    expect(Generated.EVIDENCE_PROJECTION_RUN.name).toBe("evidence.projection_run");
    expect(Generated.BANK_COMMIT.goldenInputHex).toBe(
      "86a6656e74697479ac666978747572653a62616e6ba573636f7065ad666978747572652d73636f7065ad6b696e645f63617465676f72790fac6b696e645f747970655f6964cd0a01ab7061796c6f61645f686578d93c383161353665366636653633363562363638363536313732373436323635363137343264363636393738373437353732363532643330333033303331b36964656d706f74656e63795f6b65795f686578c0",
    );
    expect(Generated.EVIDENCE_CHAIN_WALK.goldenInputHex).toBe(
      "84b273746172745f6576656e745f69645f686578d9203031323334353637383961626364656630313233343536373839616263646566b773746172745f65787065637465645f686173685f686578c0b0656e645f6576656e745f69645f686578c0a56c696d697410",
    );
    expect(Generated.EVIDENCE_STORE_RESOURCE.goldenInputHex).toBe("80");
    expect(Generated.EVIDENCE_READ_WALK.goldenInputHex).toBe(
      "89a6656e74697479ac666978747572653a62616e6ba573636f7065c0ad6b696e645f63617465676f72790fac6b696e645f747970655f6964c0ab73746172745f636c6f636bc0a9656e645f636c6f636bc0a56c696d697440b2696e636c7564655f70726f6f665f72656673c2ac6d61785f7374616c655f6d73c0",
    );
    expect(Generated.EVIDENCE_PROJECTION_RUN.goldenInputHex).toBe(
      "83aa70726f6a656374696f6eb2666978747572652e70726f6a656374696f6ea6656e74697479ac666978747572653a62616e6bac6d61785f7374616c655f6d73c0",
    );
    expect(Generated.BANK_COMMIT.errorFixture.code).toBe("unknown_operation");
  });
});
