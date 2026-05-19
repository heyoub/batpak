/**
 * Direct unit tests for @batpak/codegen.
 *
 * Exercises every validation rejection path so a malformed or
 * version-bumped manifest fails loudly at codegen time rather than
 * producing wrong TypeScript.
 */

import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { afterEach, beforeEach, describe, expect, it } from "vitest";

import {
  CodegenError,
  generate,
  readManifest,
  SUPPORTED_MANIFEST_VERSION,
  tsTypeForToken,
} from "../src/index.js";

let workDir = "";

const MINIMAL_MANIFEST = {
  manifestVersion: SUPPORTED_MANIFEST_VERSION,
  netbatVersion: "NETBAT/1",
  batpakVersion: "0.7.6",
  canonicalEncoding: {
    kind: "named-field-msgpack",
    rmpSerdeVersion: "1.3.1",
  },
  events: [
    {
      name: "test.event",
      rustType: "test::Event",
      tsName: "TestEvent",
      category: 15,
      typeId: 1,
      fields: [{ wireName: "field", tsName: "field", typeToken: "string", order: 0 }],
      fixtureValue: { field: "x" },
      goldenPayloadHex: "81a566696572746178",
    },
  ],
  operations: [
    {
      name: "test.op",
      inputEvent: "test.event",
      outputEvent: "test.event",
      inputSchemaRef: "test.event",
      outputSchemaRef: "test.event",
      receiptKind: "receipt.test.v1",
      goldenInputHex: "81a566696572746178",
      goldenOutputHex: "81a566696572746178",
      goldenRequestFrameHex: "00",
      goldenOkFrameHex: "00",
      errorFixture: {
        name: "unknown_operation",
        requestFrameHex: "00",
        errFrameHex: "00",
        code: "unknown_operation",
        messageUtf8: "test",
      },
    },
  ],
};

function writeManifest(value: unknown): string {
  const path = join(workDir, "manifest.json");
  writeFileSync(path, JSON.stringify(value, null, 2), "utf-8");
  return path;
}

beforeEach(() => {
  workDir = mkdtempSync(join(tmpdir(), "batpak-codegen-test-"));
});

afterEach(() => {
  rmSync(workDir, { recursive: true, force: true });
});

describe("readManifest validates the envelope", () => {
  it("accepts a well-formed minimal manifest", () => {
    const path = writeManifest(MINIMAL_MANIFEST);
    expect(() => readManifest(path)).not.toThrow();
  });

  it("rejects malformed JSON", () => {
    const path = join(workDir, "broken.json");
    writeFileSync(path, "{ not valid", "utf-8");
    try {
      readManifest(path);
      throw new Error("expected throw");
    } catch (error) {
      expect(error).toBeInstanceOf(CodegenError);
      if (error instanceof CodegenError) {
        expect(error.code).toBe("invalid_manifest_json");
      }
    }
  });

  it("rejects non-object roots", () => {
    const path = join(workDir, "array.json");
    writeFileSync(path, "[1,2,3]", "utf-8");
    try {
      readManifest(path);
      throw new Error("expected throw");
    } catch (error) {
      if (error instanceof CodegenError) {
        expect(error.code).toBe("invalid_manifest_shape");
      } else {
        throw error;
      }
    }
  });

  it("rejects unsupported manifestVersion", () => {
    const path = writeManifest({ ...MINIMAL_MANIFEST, manifestVersion: 99 });
    try {
      readManifest(path);
      throw new Error("expected throw");
    } catch (error) {
      if (error instanceof CodegenError) {
        expect(error.code).toBe("unsupported_manifest_version");
      } else {
        throw error;
      }
    }
  });

  it("rejects unsupported netbatVersion", () => {
    const path = writeManifest({ ...MINIMAL_MANIFEST, netbatVersion: "NETBAT/2" });
    try {
      readManifest(path);
      throw new Error("expected throw");
    } catch (error) {
      if (error instanceof CodegenError) {
        expect(error.code).toBe("unsupported_netbat_version");
      } else {
        throw error;
      }
    }
  });

  it("rejects canonicalEncoding.kind that is not named-field-msgpack", () => {
    const path = writeManifest({
      ...MINIMAL_MANIFEST,
      canonicalEncoding: { kind: "cbor", rmpSerdeVersion: "1.3.1" },
    });
    try {
      readManifest(path);
      throw new Error("expected throw");
    } catch (error) {
      if (error instanceof CodegenError) {
        expect(error.code).toBe("unsupported_canonical_encoding");
      } else {
        throw error;
      }
    }
  });

  it("rejects missing events array", () => {
    const path = writeManifest({ ...MINIMAL_MANIFEST, events: undefined });
    try {
      readManifest(path);
      throw new Error("expected throw");
    } catch (error) {
      if (error instanceof CodegenError) {
        expect(error.code).toBe("invalid_manifest_shape");
      } else {
        throw error;
      }
    }
  });

  it("rejects fields where wireName !== tsName", () => {
    const path = writeManifest({
      ...MINIMAL_MANIFEST,
      events: [
        {
          ...MINIMAL_MANIFEST.events[0],
          fields: [
            {
              wireName: "snake_case",
              tsName: "camelCase",
              typeToken: "string",
              order: 0,
            },
          ],
        },
      ],
    });
    try {
      readManifest(path);
      throw new Error("expected throw");
    } catch (error) {
      if (error instanceof CodegenError) {
        expect(error.code).toBe("field_name_drift");
      } else {
        throw error;
      }
    }
  });

  it("rejects unknown typeToken values", () => {
    const path = writeManifest({
      ...MINIMAL_MANIFEST,
      events: [
        {
          ...MINIMAL_MANIFEST.events[0],
          fields: [{ wireName: "x", tsName: "x", typeToken: "geocode", order: 0 }],
        },
      ],
    });
    try {
      readManifest(path);
      throw new Error("expected throw");
    } catch (error) {
      if (error instanceof CodegenError) {
        expect(error.code).toBe("unsupported_field_type");
      } else {
        throw error;
      }
    }
  });
});

describe("generate writes the expected files", () => {
  it("creates events.ts, operations.ts, manifest.ts, index.ts", () => {
    const path = writeManifest(MINIMAL_MANIFEST);
    const out = join(workDir, "generated-out");
    generate({ manifestPath: path, outDir: out });
    expect(existsSync(join(out, "events.ts"))).toBe(true);
    expect(existsSync(join(out, "operations.ts"))).toBe(true);
    expect(existsSync(join(out, "manifest.ts"))).toBe(true);
    expect(existsSync(join(out, "index.ts"))).toBe(true);
  });

  it("FULLY OVERWRITES the output directory on each run", () => {
    const path = writeManifest(MINIMAL_MANIFEST);
    const out = join(workDir, "generated-out");
    // Pre-populate with a stray file that codegen should remove.
    mkdirSync(out, { recursive: true });
    writeFileSync(join(out, "stray.ts"), "// stray", "utf-8");
    generate({ manifestPath: path, outDir: out });
    expect(existsSync(join(out, "stray.ts"))).toBe(false);
  });

  it("emits a fixture object whose key order matches the fields array", () => {
    // Two-field event with a non-alphabetical declaration order.
    const manifest = {
      ...MINIMAL_MANIFEST,
      events: [
        {
          name: "x.e",
          rustType: "x::E",
          tsName: "XE",
          category: 15,
          typeId: 2,
          fields: [
            { wireName: "zebra", tsName: "zebra", typeToken: "string", order: 0 },
            { wireName: "apple", tsName: "apple", typeToken: "string", order: 1 },
          ],
          fixtureValue: { apple: "a", zebra: "z" }, // alphabetical from BTreeMap
          goldenPayloadHex: "00",
        },
      ],
      operations: [],
    };
    const path = writeManifest(manifest);
    const out = join(workDir, "out");
    generate({ manifestPath: path, outDir: out });
    const events = readFileSync(join(out, "events.ts"), "utf-8");
    // The fixture object literal should have zebra FIRST in the emitted
    // source, since fields[0].wireName = "zebra".
    const idxZebra = events.indexOf('"zebra"');
    const idxApple = events.indexOf('"apple"');
    expect(idxZebra).toBeGreaterThan(-1);
    expect(idxApple).toBeGreaterThan(idxZebra);
  });
});

describe("token vocabulary mapping", () => {
  it("maps every supported token to a TS type", () => {
    const tokens = [
      "string",
      "u8",
      "u16",
      "u32",
      "u64-safe",
      "u64-millis",
      "i64-microseconds",
      "option<string>",
      "map<string,string>",
    ] as const;
    const expected: Record<(typeof tokens)[number], string> = {
      string: "string",
      u8: "number",
      u16: "number",
      u32: "number",
      "u64-safe": "number",
      "u64-millis": "number",
      "i64-microseconds": "number",
      "option<string>": "string | null",
      "map<string,string>": "Record<string, string>",
    };
    for (const t of tokens) {
      expect(tsTypeForToken(t)).toBe(expected[t]);
    }
  });

  it("throws on unknown tokens with a CodegenError carrying unsupported_field_type", () => {
    try {
      tsTypeForToken("doubloon");
      throw new Error("expected throw");
    } catch (error) {
      expect(error).toBeInstanceOf(CodegenError);
      if (error instanceof CodegenError) {
        expect(error.code).toBe("unsupported_field_type");
      }
    }
  });
});
