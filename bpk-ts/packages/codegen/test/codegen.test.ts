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
  schemaForToken,
  SUPPORTED_MANIFEST_VERSION,
  tsTypeForToken,
} from "../src/index.js";

let workDir = "";

const MINIMAL_MANIFEST = {
  manifestVersion: SUPPORTED_MANIFEST_VERSION,
  netbatVersion: "NETBAT/1",
  batpakVersion: "0.8.0",
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

  it("accepts option<blake3-32-hex> for evidence chain-walk anchors", () => {
    const path = writeManifest({
      ...MINIMAL_MANIFEST,
      events: [
        {
          ...MINIMAL_MANIFEST.events[0],
          name: "evidence.chain_walk.request",
          tsName: "EvidenceChainWalkRequest",
          fields: [
            {
              wireName: "start_expected_hash_hex",
              tsName: "start_expected_hash_hex",
              typeToken: "option<blake3-32-hex>",
              order: 0,
            },
          ],
          fixtureValue: { start_expected_hash_hex: null },
          goldenPayloadHex: "c0",
        },
      ],
    });
    expect(() => readManifest(path)).not.toThrow();
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

  it("does not leak the host absolute path of the manifest into manifest.ts", () => {
    // REGRESSION (CI determinism gate on commit c67394b):
    // `cli.ts` resolves the user-supplied manifest path with
    // `path.resolve(...)` before calling `generate`, and
    // `renderManifestModule` used to emit `${options.manifestPath}`
    // verbatim in a `// Source manifest: ...` comment. That leaks
    // the host filesystem layout into the generated file —
    // `/home/user/...` on a developer laptop vs
    // `/home/runner/work/...` on GitHub-hosted CI — so the
    // determinism gate (rm -rf generated && rebuild && diff)
    // would deterministically fail any time the file was
    // regenerated on a different host. Now the comment carries
    // only `basename(options.manifestPath)`.
    const path = writeManifest(MINIMAL_MANIFEST);
    const out = join(workDir, "generated-path-leak");
    generate({ manifestPath: path, outDir: out });
    const manifestTs = readFileSync(join(out, "manifest.ts"), "utf-8");
    expect(manifestTs).toContain("// Source manifest: ");
    expect(manifestTs).not.toContain(workDir);
    expect(manifestTs).not.toMatch(/Source manifest: \//u);
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

  it("emits valid TS identifiers even when the operation name contains hyphens", () => {
    // REGRESSION (Codex P2 on commit c6fdfdb): renderOperationsModule
    // used to strip only `.` from op.name when forming the const
    // identifier. Hyphens were left in place, producing illegal TS
    // like `export const BANK-COMMIT = ...`. The OperationName
    // grammar allows `[A-Za-z0-9._-]+`, so hyphens must collapse to
    // underscores too.
    const manifest = {
      ...MINIMAL_MANIFEST,
      operations: [
        {
          ...MINIMAL_MANIFEST.operations[0],
          name: "bank-commit.v2",
        },
      ],
    };
    const path = writeManifest(manifest);
    const out = join(workDir, "out");
    generate({ manifestPath: path, outDir: out });
    const ops = readFileSync(join(out, "operations.ts"), "utf-8");
    expect(ops).toContain("export const BANK_COMMIT_V2");
    expect(ops).not.toContain("BANK-COMMIT");
  });

  it("emits valid TS identifiers when the operation name starts with a digit", () => {
    // REGRESSION (Codex P2 on commit e6279a3): the
    // OperationName grammar `[A-Za-z0-9._-]+` allows a leading digit
    // (e.g. `1.sync`). Uppercasing produces `1_SYNC` — invalid TS
    // because JS identifiers cannot start with [0-9]. The fix
    // prefixes `_` so the emitted constant becomes `_1_SYNC`.
    const manifest = {
      ...MINIMAL_MANIFEST,
      operations: [
        {
          ...MINIMAL_MANIFEST.operations[0],
          name: "1.sync",
        },
      ],
    };
    const path = writeManifest(manifest);
    const out = join(workDir, "out");
    generate({ manifestPath: path, outDir: out });
    const ops = readFileSync(join(out, "operations.ts"), "utf-8");
    expect(ops).toContain("export const _1_SYNC");
    // The illegal `export const 1_SYNC` MUST NOT appear.
    expect(ops).not.toMatch(/export const 1_SYNC\b/u);
  });

  it("fails with operation_name_collision when two ops normalize to the same TS const", () => {
    // REGRESSION (Codex P2 on commit 62cff8e): `"bank.commit"` and
    // `"bank-commit"` are both valid OperationName wire shapes (the
    // grammar accepts both `.` and `-`), but they collapse to the
    // same `BANK_COMMIT` TS identifier under the dot+hyphen
    // normalization. Without a uniqueness check, the codegen would
    // emit duplicate `export const BANK_COMMIT = ...` lines and the
    // generated package would fail to compile. Now we catch it at
    // codegen time with operation_name_collision.
    const manifest = {
      ...MINIMAL_MANIFEST,
      operations: [
        { ...MINIMAL_MANIFEST.operations[0], name: "bank.commit" },
        { ...MINIMAL_MANIFEST.operations[0], name: "bank-commit" },
      ],
    };
    const path = writeManifest(manifest);
    const out = join(workDir, "out");
    try {
      generate({ manifestPath: path, outDir: out });
      throw new Error("expected operation_name_collision");
    } catch (error) {
      expect(error).toBeInstanceOf(CodegenError);
      if (error instanceof CodegenError) {
        expect(error.code).toBe("operation_name_collision");
      }
    }
  });

  it("dedupes shared event type aliases across operations", () => {
    // REGRESSION (Codex P2 on commit 62cff8e): each operation used
    // to emit `export type <EventTsName>Input = ...` inside the
    // per-op loop. Two operations sharing the same input event
    // type (legitimate — same request schema, different effect
    // class) would produce two identical `export type FooInput`
    // declarations and tsc would fail to compile the generated
    // package. Now the aliases are accumulated into a deduped Map
    // and emitted ONCE after the loop.
    const sharedEvent = {
      ...MINIMAL_MANIFEST.events[0],
      name: "shared.event",
      tsName: "SharedEvent",
    };
    const manifest = {
      ...MINIMAL_MANIFEST,
      events: [sharedEvent],
      operations: [
        {
          ...MINIMAL_MANIFEST.operations[0],
          name: "op.alpha",
          inputEvent: "shared.event",
          outputEvent: "shared.event",
        },
        {
          ...MINIMAL_MANIFEST.operations[0],
          name: "op.beta",
          inputEvent: "shared.event",
          outputEvent: "shared.event",
        },
      ],
    };
    const path = writeManifest(manifest);
    const out = join(workDir, "out");
    generate({ manifestPath: path, outDir: out });
    const ops = readFileSync(join(out, "operations.ts"), "utf-8");
    // Each unique alias appears EXACTLY ONCE, never twice.
    const inputMatches = ops.match(/export type SharedEventInput =/gu) ?? [];
    const outputMatches = ops.match(/export type SharedEventOutput =/gu) ?? [];
    expect(inputMatches).toHaveLength(1);
    expect(outputMatches).toHaveLength(1);
  });

  it("fails with event_ts_name_collision when two events share a tsName", () => {
    // REGRESSION (Codex P2 on commit ec4898f): renderEventsModule
    // emits `export const ${tsName}` AND `export type ${tsName}` for
    // every manifest event without checking `tsName` uniqueness.
    // Manifest validation does not enforce this either. Two events
    // sharing a `tsName` (e.g. two Rust modules each defining a type
    // that maps to `Heartbeat`) would produce duplicate identifiers
    // and break tsc. Now we surface a precise CodegenError naming
    // both offending Rust types, so the operator can disambiguate at
    // the source instead of debugging a duplicate-export tsc error.
    const manifest = {
      ...MINIMAL_MANIFEST,
      events: [
        {
          ...MINIMAL_MANIFEST.events[0],
          name: "alpha.heartbeat",
          rustType: "alpha::Heartbeat",
          tsName: "Heartbeat",
        },
        {
          ...MINIMAL_MANIFEST.events[0],
          name: "beta.heartbeat",
          rustType: "beta::Heartbeat",
          tsName: "Heartbeat",
          typeId: 2,
        },
      ],
      operations: [],
    };
    const path = writeManifest(manifest);
    const out = join(workDir, "out");
    try {
      generate({ manifestPath: path, outDir: out });
      throw new Error("expected event_ts_name_collision");
    } catch (error) {
      expect(error).toBeInstanceOf(CodegenError);
      if (error instanceof CodegenError) {
        expect(error.code).toBe("event_ts_name_collision");
      }
    }
  });

  it("rejects manifests where an event tsName collides with a generated type alias", () => {
    // REGRESSION (Codex P2 on commit 1497d94): operations.ts imports
    // every event tsName from ./events.js, then emits
    // `export type <RequestEvent>Input = <RequestEvent>`. If the
    // manifest ALSO has an event whose tsName is literally
    // `FooInput`, the alias declaration collides with the imported
    // event type (TS2440 duplicate identifier) and the consumer
    // package fails to compile.
    //
    // Manifest below: two events `Foo` and `FooInput`, plus one
    // operation whose inputEvent is `Foo`. The naive emitter would
    // produce `export type FooInput = Foo` while `FooInput` is
    // already imported. We want a CodegenError up front.
    const manifest = {
      ...MINIMAL_MANIFEST,
      events: [
        {
          ...MINIMAL_MANIFEST.events[0],
          name: "foo",
          tsName: "Foo",
        },
        {
          ...MINIMAL_MANIFEST.events[0],
          name: "foo.input",
          tsName: "FooInput",
          typeId: 2,
        },
      ],
      operations: [
        {
          ...MINIMAL_MANIFEST.operations[0],
          name: "use.foo",
          inputEvent: "foo",
          outputEvent: "foo",
        },
      ],
    };
    const path = writeManifest(manifest);
    const out = join(workDir, "out");
    try {
      generate({ manifestPath: path, outDir: out });
      throw new Error("expected type_alias_event_name_collision");
    } catch (error) {
      expect(error).toBeInstanceOf(CodegenError);
      if (error instanceof CodegenError) {
        expect(error.code).toBe("type_alias_event_name_collision");
      }
    }
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
      "u64-safe-positive",
      "u64-millis",
      "i64-microseconds",
      "option<string>",
      "option<u128-hex>",
      "option<u8>",
      "option<u16>",
      "option<u32>",
      "option<u64-safe>",
      "option<u64-safe-positive>",
      "bool",
      "map<string,string>",
      "array<EventSummary>",
      "u128-hex",
      "blake3-32-hex",
      "ed25519-sig-hex",
      "key-id-hex",
      "option<ed25519-sig-hex>",
      "option<blake3-32-hex>",
      "hex-blob",
      "map<string,hex-blob>",
    ] as const;
    const expected: Record<(typeof tokens)[number], string> = {
      string: "string",
      u8: "number",
      u16: "number",
      u32: "number",
      "u64-safe": "number",
      "u64-safe-positive": "number",
      "u64-millis": "number",
      "i64-microseconds": "number",
      "option<string>": "string | null",
      "option<u128-hex>": '(string & Schema.Brand<"EventIdHex">) | null',
      "option<u8>": "number | null",
      "option<u16>": "number | null",
      "option<u32>": "number | null",
      "option<u64-safe>": "number | null",
      "option<u64-safe-positive>": "number | null",
      bool: "boolean",
      "map<string,string>": "Record<string, string>",
      "array<EventSummary>": "Array<EventSummary>",
      "u128-hex": 'string & Schema.Brand<"EventIdHex">',
      "blake3-32-hex": 'string & Schema.Brand<"ContentHashHex">',
      "ed25519-sig-hex": 'string & Schema.Brand<"SignatureHex">',
      "key-id-hex": 'string & Schema.Brand<"KeyIdHex">',
      "option<ed25519-sig-hex>": '(string & Schema.Brand<"SignatureHex">) | null',
      "option<blake3-32-hex>": '(string & Schema.Brand<"ContentHashHex">) | null',
      "hex-blob": 'string & Schema.Brand<"HexBlob">',
      "map<string,hex-blob>": 'Record<string, string & Schema.Brand<"HexBlob">>',
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

describe("schemaForToken ranges", () => {
  it("emits checked bounds for option<u32> and option<u64-safe-positive>", () => {
    expect(schemaForToken("option<u32>")).toContain("minimum: 0, maximum: 4294967295");
    expect(schemaForToken("option<u64-safe-positive>")).toContain(
      "minimum: 1, maximum: 9007199254740991",
    );
  });

  it("keeps option<u64-safe> allowing zero", () => {
    expect(schemaForToken("option<u64-safe>")).toContain("minimum: 0, maximum: 9007199254740991");
  });
});
