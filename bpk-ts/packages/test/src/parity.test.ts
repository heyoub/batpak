/**
 * BatPAK TS SDK Phase 0 parity harness.
 *
 * Asserts byte-for-byte parity between the Rust-generated manifest
 * (`bpk-ts/batpak.manifest.json`) and the TypeScript canonical codec +
 * NETBAT/1 frame client.
 *
 * Acceptance categories (see plan):
 *   1. Payload decode parity
 *   2. Payload encode parity
 *   3. Frame parse parity (CALL + OK + ERR)
 *   4. Frame emit parity (CALL request)
 *   5. Manifest-version guard
 *   6. ERR fixture: code + UTF-8 message decode, NOT MessagePack
 *   7. Denial-vocabulary guard (no ReceiptOutcome::Denied in ERR path)
 *   8. wireName === tsName Phase 0 invariant
 */

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";
import { describe, expect, it } from "vitest";

import { decode, decodeHex, encode, encodeHex } from "@batpak/canonical";
import {
  encodeRequest,
  parseRequestFrame,
  parseResponseFrame,
  NETBAT_ERROR_CODES,
} from "@batpak/client";
import {
  readManifest,
  type BatpakTsManifest,
  type ManifestEvent,
  type ManifestOperation,
} from "@batpak/codegen";

const here = dirname(fileURLToPath(import.meta.url));
const MANIFEST_PATH = resolve(here, "../../../batpak.manifest.json");
const manifest: BatpakTsManifest = readManifest(MANIFEST_PATH);

function eventByName(name: string): ManifestEvent {
  const event = manifest.events.find((e) => e.name === name);
  if (!event) throw new Error(`manifest missing event ${name}`);
  return event;
}

function operation(name: string): ManifestOperation {
  const op = manifest.operations.find((o) => o.name === name);
  if (!op) throw new Error(`manifest missing operation ${name}`);
  return op;
}

describe("manifest envelope", () => {
  it("declares the Phase 0 protocol versions", () => {
    expect(manifest.manifestVersion).toBe(1);
    expect(manifest.netbatVersion).toBe("NETBAT/1");
    expect(manifest.canonicalEncoding.kind).toBe("named-field-msgpack");
    expect(manifest.canonicalEncoding.rmpSerdeVersion).toBe("1.3.1");
  });

  it("enforces the Phase 0 wireName === tsName invariant", () => {
    for (const event of manifest.events) {
      for (const field of event.fields) {
        expect(field.wireName).toBe(field.tsName);
      }
    }
  });
});

describe("payload encode/decode parity", () => {
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

describe("frame emit parity", () => {
  it("encodes the system.heartbeat CALL frame to goldenRequestFrameHex", () => {
    const op = operation("system.heartbeat");
    const inputBytes = decodeHex(op.goldenInputHex);
    const frame = encodeRequest(op.name, inputBytes);
    expect(encodeHex(frame)).toBe(op.goldenRequestFrameHex);
  });
});

describe("frame parse parity", () => {
  it("parses goldenRequestFrameHex back to (operation, payload)", () => {
    const op = operation("system.heartbeat");
    const parsed = parseRequestFrame(decodeHex(op.goldenRequestFrameHex));
    expect(parsed.operation).toBe(op.name);
    expect(encodeHex(parsed.input)).toBe(op.goldenInputHex);
  });

  it("parses goldenOkFrameHex into a NetbatOk carrying goldenOutputHex", () => {
    const op = operation("system.heartbeat");
    const parsed = parseResponseFrame(decodeHex(op.goldenOkFrameHex));
    expect(parsed.kind).toBe("netbat-ok");
    if (parsed.kind !== "netbat-ok") return;
    expect(encodeHex(parsed.output)).toBe(op.goldenOutputHex);
  });

  it("parses errorFixture.errFrameHex into a typed NetbatError", () => {
    const op = operation("system.heartbeat");
    const parsed = parseResponseFrame(decodeHex(op.errorFixture.errFrameHex));
    expect(parsed.kind).toBe("netbat-error");
    if (parsed.kind !== "netbat-error") return;
    expect(parsed.code).toBe("unknown_operation");
    expect(parsed.message).toBe(op.errorFixture.messageUtf8);
  });
});

describe("ERR fixture UTF-8 message guard", () => {
  it("treats the message as UTF-8 text, never MessagePack", () => {
    const op = operation("system.heartbeat");
    const parsed = parseResponseFrame(decodeHex(op.errorFixture.errFrameHex));
    if (parsed.kind !== "netbat-error") {
      throw new Error("expected NetbatError");
    }
    expect(() => decode(new TextEncoder().encode(parsed.message))).toThrow();
  });
});

describe("denial-vocabulary guard", () => {
  it("ERR code is in the netbat union, never ReceiptOutcome::Denied", () => {
    const op = operation("system.heartbeat");
    const parsed = parseResponseFrame(decodeHex(op.errorFixture.errFrameHex));
    if (parsed.kind !== "netbat-error") {
      throw new Error("expected NetbatError");
    }
    expect(NETBAT_ERROR_CODES).toContain(parsed.code);
    expect(parsed.message.toLowerCase()).not.toContain("denied");
    expect(parsed.message.toLowerCase()).not.toContain("denial");
  });
});

describe("manifest-version guard", () => {
  it("readManifest accepts manifestVersion: 1", () => {
    expect(() => readManifest(MANIFEST_PATH)).not.toThrow();
  });

  it("readManifest refuses an unsupported manifestVersion", async () => {
    const { writeFileSync, mkdtempSync, rmSync } = await import("node:fs");
    const { tmpdir } = await import("node:os");
    const { join } = await import("node:path");
    const tmp = mkdtempSync(join(tmpdir(), "batpak-ts-manifest-"));
    try {
      const path = join(tmp, "batpak.manifest.json");
      const raw = readFileSync(MANIFEST_PATH, "utf-8");
      const tampered = raw.replace(`"manifestVersion": 1`, `"manifestVersion": 99`);
      writeFileSync(path, tampered, "utf-8");
      expect(() => readManifest(path)).toThrow(
        /manifestVersion 99 is not supported/,
      );
    } finally {
      rmSync(tmp, { recursive: true, force: true });
    }
  });
});

describe("event-by-name lookups (sanity)", () => {
  it("finds both heartbeat events", () => {
    expect(eventByName("system.heartbeat.request").tsName).toBe(
      "SystemHeartbeatRequest",
    );
    expect(eventByName("system.heartbeat.ack").tsName).toBe(
      "SystemHeartbeatAck",
    );
  });
});
