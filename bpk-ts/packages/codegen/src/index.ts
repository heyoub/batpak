/**
 * BatPAK TypeScript codegen.
 *
 * Reads `batpak.manifest.json` and emits TypeScript symbols into a fresh
 * `packages/generated/src` tree. Output is FULLY OVERWRITTEN on each
 * run; the codegen never patches existing files.
 *
 * Each manifest event produces three exports in `events.ts`:
 *   - `export const X = Schema.Struct({...})` — the Effect 4 Schema
 *     (runtime validation + serialization shape).
 *   - `export type X = typeof X.Type` — the TS interface derived from
 *     the schema.
 *   - `export const X_GOLDEN_HEX` / `X_FIXTURE` — golden test data.
 *
 * Each manifest operation produces an `operations.ts` entry binding the
 * op name + golden frames + error fixture + the request/response
 * Schema references.
 */

import { mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { dirname, basename, join, resolve } from "node:path";

export const SUPPORTED_MANIFEST_VERSION = 1;

/**
 * Phase 0 wire-token vocabulary. Adding a new entry here requires:
 *   1. Updating [`tsTypeForToken`] (the plain TS type lane).
 *   2. Updating [`schemaForToken`] (the Effect 4 Schema lane).
 *   3. Ensuring the Rust manifest exporter actually emits it.
 */
const SUPPORTED_FIELD_TYPES = new Set<string>([
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
  "option<u64-safe>",
  "bool",
  "map<string,string>",
  "array<EventSummary>",
  // Branded hex tokens. Each emits a Schema.String guarded by a pattern
  // refinement and a Schema.brand("…") so passing a wrong hex shape
  // (e.g. an event_id where a content hash was expected) fails at the
  // type checker — not just at runtime.
  "u128-hex", // 32 lowercase hex chars
  "blake3-32-hex", // 64 lowercase hex chars
  "ed25519-sig-hex", // 128 lowercase hex chars
  "key-id-hex", // 64 lowercase hex chars (Ed25519 verifier identity)
  "option<ed25519-sig-hex>",
  "option<blake3-32-hex>",
  // Free-form hex payload (variable length, lowercase). Branded so
  // callers can prove "I built this via the canonical encoder" at the
  // type system, but not constrained to a fixed length.
  "hex-blob",
  // Receipt-extension map: keys are extension keys
  // (`namespace.field`), values are hex-blobs. Same shape as the
  // existing `map<string,string>` but the value type is the branded
  // hex blob.
  "map<string,hex-blob>",
]);

export interface ManifestField {
  wireName: string;
  tsName: string;
  typeToken: string;
  order: number;
}

export interface ManifestEvent {
  name: string;
  rustType: string;
  tsName: string;
  category: number;
  typeId: number;
  fields: ManifestField[];
  fixtureValue: unknown;
  goldenPayloadHex: string;
}

export interface ManifestErrorFixture {
  name: string;
  requestFrameHex: string;
  errFrameHex: string;
  code: string;
  messageUtf8: string;
}

export interface ManifestOperation {
  name: string;
  inputEvent: string;
  outputEvent: string;
  inputSchemaRef: string;
  outputSchemaRef: string;
  receiptKind: string;
  goldenInputHex: string;
  goldenOutputHex: string;
  goldenRequestFrameHex: string;
  goldenOkFrameHex: string;
  errorFixture: ManifestErrorFixture;
}

export interface BatpakTsManifest {
  manifestVersion: number;
  netbatVersion: string;
  batpakVersion: string;
  canonicalEncoding: {
    kind: string;
    rmpSerdeVersion: string;
  };
  events: ManifestEvent[];
  operations: ManifestOperation[];
}

export class CodegenError extends Error {
  readonly code: string;
  constructor(code: string, message: string) {
    super(message);
    this.name = "CodegenError";
    this.code = code;
  }
}

export function readManifest(path: string): BatpakTsManifest {
  const raw = readFileSync(path, "utf-8");
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch (error) {
    throw new CodegenError(
      "invalid_manifest_json",
      `${path}: not valid JSON (${(error as Error).message})`,
    );
  }
  return validateManifest(parsed, path);
}

function validateManifest(value: unknown, source: string): BatpakTsManifest {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new CodegenError(
      "invalid_manifest_shape",
      `${source}: manifest root must be a JSON object`,
    );
  }
  const manifest = value as Partial<BatpakTsManifest>;
  if (manifest.manifestVersion !== SUPPORTED_MANIFEST_VERSION) {
    throw new CodegenError(
      "unsupported_manifest_version",
      `${source}: manifestVersion ${manifest.manifestVersion} is not supported (codegen supports ${SUPPORTED_MANIFEST_VERSION})`,
    );
  }
  if (manifest.netbatVersion !== "NETBAT/1") {
    throw new CodegenError(
      "unsupported_netbat_version",
      `${source}: netbatVersion ${JSON.stringify(manifest.netbatVersion)} is not supported (codegen supports NETBAT/1)`,
    );
  }
  if (
    typeof manifest.canonicalEncoding !== "object" ||
    manifest.canonicalEncoding === null ||
    manifest.canonicalEncoding.kind !== "named-field-msgpack"
  ) {
    throw new CodegenError(
      "unsupported_canonical_encoding",
      `${source}: canonicalEncoding.kind must be "named-field-msgpack"`,
    );
  }
  if (!Array.isArray(manifest.events) || !Array.isArray(manifest.operations)) {
    throw new CodegenError(
      "invalid_manifest_shape",
      `${source}: manifest must contain events[] and operations[] arrays`,
    );
  }
  for (const event of manifest.events) {
    for (const field of event.fields) {
      if (field.wireName !== field.tsName) {
        throw new CodegenError(
          "field_name_drift",
          `${source}: ${event.name}.${field.wireName}: Phase 0 invariant wireName === tsName violated (tsName=${field.tsName})`,
        );
      }
      if (!SUPPORTED_FIELD_TYPES.has(field.typeToken)) {
        throw new CodegenError(
          "unsupported_field_type",
          `${source}: ${event.name}.${field.wireName} uses unsupported typeToken ${JSON.stringify(field.typeToken)}; supported: ${[...SUPPORTED_FIELD_TYPES].join(", ")}`,
        );
      }
    }
  }
  return manifest as BatpakTsManifest;
}

export interface GenerateOptions {
  manifestPath: string;
  outDir: string;
}

export function generate(options: GenerateOptions): void {
  const manifest = readManifest(options.manifestPath);
  const outDir = resolve(options.outDir);
  rmSync(outDir, { recursive: true, force: true });
  mkdirSync(outDir, { recursive: true });

  writeFile(join(outDir, "manifest.ts"), renderManifestModule(manifest, options));
  writeFile(join(outDir, "events.ts"), renderEventsModule(manifest));
  writeFile(join(outDir, "operations.ts"), renderOperationsModule(manifest));
  writeFile(join(outDir, "index.ts"), renderIndexModule());
}

function writeFile(path: string, contents: string): void {
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, contents, "utf-8");
}

const FILE_HEADER = `// AUTO-GENERATED by @batpak/codegen from batpak.manifest.json. DO NOT EDIT.
// Re-run \`pnpm -w generate\` (or \`cargo xtask export-ts-manifest\` followed
// by codegen) to refresh this directory.
`;

function renderManifestModule(manifest: BatpakTsManifest, options: GenerateOptions): string {
  const json = JSON.stringify(manifest, null, 2);
  // Emit only the basename in the source-of-record comment. The CLI
  // resolves the user-supplied manifest path to absolute before
  // handing it to `generate`, which would otherwise leak the host
  // filesystem layout into the generated file and break the CI
  // determinism gate (`/home/user/...` locally vs
  // `/home/runner/work/...` on GitHub-hosted runners).
  return [
    FILE_HEADER,
    `// Source manifest: ${basename(options.manifestPath)}`,
    `export const MANIFEST_VERSION = ${manifest.manifestVersion} as const;`,
    `export const NETBAT_VERSION = ${JSON.stringify(manifest.netbatVersion)} as const;`,
    `export const BATPAK_VERSION = ${JSON.stringify(manifest.batpakVersion)} as const;`,
    `export const CANONICAL_ENCODING = ${JSON.stringify(manifest.canonicalEncoding)} as const;`,
    `export const BATPAK_TS_MANIFEST = ${json} as const;`,
    "",
  ].join("\n");
}

function renderEventsModule(manifest: BatpakTsManifest): string {
  const lines: string[] = [FILE_HEADER, `import * as Schema from "effect/Schema";`, ""];
  // Manifest validation does not enforce `tsName` uniqueness, but we
  // emit `export const ${tsName}` AND `export type ${tsName}` for
  // every event — two events sharing a `tsName` would produce
  // duplicate identifiers and break `tsc`. Fail loudly at codegen
  // time with the offending Rust types named, so the operator can
  // disambiguate at the source.
  const seenTsNames = new Map<string, string>();
  for (const event of manifest.events) {
    const previousRustType = seenTsNames.get(event.tsName);
    if (previousRustType !== undefined) {
      throw new CodegenError(
        "event_ts_name_collision",
        `events ${JSON.stringify(previousRustType)} and ${JSON.stringify(event.rustType)} both declare tsName ${JSON.stringify(event.tsName)}; pick distinct tsNames so the generated events.ts compiles`,
      );
    }
    seenTsNames.set(event.tsName, event.rustType);
    lines.push(
      `/** Source: ${event.rustType}; category=${event.category}, typeId=${event.typeId} */`,
    );
    lines.push(`export const ${event.tsName} = Schema.Struct({`);
    for (const field of event.fields) {
      lines.push(`  ${field.wireName}: ${schemaForToken(field.typeToken)},`);
    }
    lines.push(`});`);
    // Type alias derived from the schema (same name; lives in type
    // namespace).
    lines.push(`// eslint-disable-next-line @typescript-eslint/no-redeclare`);
    lines.push(`export type ${event.tsName} = typeof ${event.tsName}.Type;`);
    lines.push("");

    const constSafeName = constCase(event.tsName);
    // CRITICAL: Rust's rmp-serde::to_vec_named emits struct fields in
    // DECLARATION order; serde_json::to_value uses BTreeMap (alphabetical).
    // The TS canonical encoder iterates object insertion order. So the
    // fixture object literal MUST have keys in declaration order — i.e.
    // the order of the `fields` array — to round-trip against the golden
    // hex.
    const orderedFixture = reorderObjectByFields(event.fixtureValue, event.fields);
    lines.push(
      `export const ${constSafeName}_GOLDEN_HEX = ${JSON.stringify(event.goldenPayloadHex)} as const;`,
    );
    lines.push(
      // Fixture literal is cast through `unknown` to the event's
      // branded shape. Brands are phantom types — sound at runtime,
      // but TS treats `string` and `string & Brand<...>` as
      // non-overlapping for direct casts, so the double cast is the
      // documented escape hatch.
      `export const ${constSafeName}_FIXTURE: ${event.tsName} = ${JSON.stringify(orderedFixture, null, 2)} as unknown as ${event.tsName};`,
    );
    lines.push("");
  }
  return lines.join("\n");
}

/**
 * Reorder a JSON-shaped fixture object so its keys appear in the
 * declaration order specified by `fields[*].wireName`. Required because
 * the Rust manifest exporter goes through `serde_json::to_value` which
 * loses declaration order to BTreeMap (alphabetical).
 *
 * Unknown keys (none expected) are preserved at the end in their
 * original order, so a future field added in Rust but not yet in
 * `fields` would still appear.
 */
function reorderObjectByFields(value: unknown, fields: readonly ManifestField[]): unknown {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    return value;
  }
  const obj = value as Record<string, unknown>;
  const out: Record<string, unknown> = {};
  for (const field of fields) {
    if (field.wireName in obj) {
      out[field.wireName] = obj[field.wireName];
    }
  }
  for (const key of Object.keys(obj)) {
    if (!(key in out)) {
      out[key] = obj[key];
    }
  }
  return out;
}

function renderOperationsModule(manifest: BatpakTsManifest): string {
  const lines: string[] = [
    FILE_HEADER,
    `import type { ${manifest.events.map((e) => e.tsName).join(", ")} } from "./events.js";`,
    "",
  ];
  // Track sanitized identifiers across operations so collisions
  // (`bank.commit` and `bank-commit` both normalize to BANK_COMMIT)
  // fail at codegen time with a precise diagnostic instead of
  // producing duplicate `export const BANK_COMMIT = ...` lines that
  // tsc can't compile.
  const seenConstNames = new Map<string, string>();
  // Dedupe per-event-tsName type-alias emission so two operations
  // sharing an input or output event type don't produce duplicate
  // `export type X = Y` lines. Each unique alias gets emitted once
  // AFTER the operations loop.
  const inputAliases = new Map<string, string>();
  const outputAliases = new Map<string, string>();

  for (const op of manifest.operations) {
    const requestEvent = manifest.events.find((e) => e.name === op.inputEvent);
    const responseEvent = manifest.events.find((e) => e.name === op.outputEvent);
    if (!requestEvent || !responseEvent) {
      throw new CodegenError(
        "invalid_manifest_shape",
        `operation ${op.name}: missing referenced event (${op.inputEvent} or ${op.outputEvent})`,
      );
    }
    // The OperationName grammar (syncbat::OperationName::new) accepts
    // `[A-Za-z0-9._-]+`, which means valid wire names can also START
    // with a digit (e.g. "1.sync") or contain hyphens. The TS const
    // identifier we emit MUST be a valid JS identifier, so:
    //   1. collapse every separator the grammar allows (dot AND
    //      hyphen) into underscore — otherwise `bank-commit.v2`
    //      becomes `export const BANK-COMMIT = ...` (invalid TS),
    //   2. prefix `_` when the result starts with a digit — JS
    //      identifiers cannot start with `[0-9]`, so `1.sync` would
    //      become `export const 1_SYNC = ...` (invalid TS).
    let constName = constCase(op.name.replace(/[.-]/gu, "_")).toUpperCase();
    if (/^[0-9]/u.test(constName)) {
      constName = `_${constName}`;
    }
    const previous = seenConstNames.get(constName);
    if (previous !== undefined) {
      throw new CodegenError(
        "operation_name_collision",
        `operations ${JSON.stringify(previous)} and ${JSON.stringify(op.name)} both normalize to the TS constant identifier ${constName}; pick names that survive the [./-] -> _ collapse to be distinct`,
      );
    }
    seenConstNames.set(constName, op.name);

    lines.push(`/** Source: syncbat operation "${op.name}" */`);
    lines.push(`export const ${constName} = {`);
    lines.push(`  name: ${JSON.stringify(op.name)},`);
    lines.push(`  inputEvent: ${JSON.stringify(op.inputEvent)},`);
    lines.push(`  outputEvent: ${JSON.stringify(op.outputEvent)},`);
    lines.push(`  inputSchemaRef: ${JSON.stringify(op.inputSchemaRef)},`);
    lines.push(`  outputSchemaRef: ${JSON.stringify(op.outputSchemaRef)},`);
    lines.push(`  receiptKind: ${JSON.stringify(op.receiptKind)},`);
    lines.push(`  goldenInputHex: ${JSON.stringify(op.goldenInputHex)},`);
    lines.push(`  goldenOutputHex: ${JSON.stringify(op.goldenOutputHex)},`);
    lines.push(`  goldenRequestFrameHex: ${JSON.stringify(op.goldenRequestFrameHex)},`);
    lines.push(`  goldenOkFrameHex: ${JSON.stringify(op.goldenOkFrameHex)},`);
    lines.push(`  errorFixture: {`);
    lines.push(`    name: ${JSON.stringify(op.errorFixture.name)},`);
    lines.push(`    code: ${JSON.stringify(op.errorFixture.code)},`);
    lines.push(`    requestFrameHex: ${JSON.stringify(op.errorFixture.requestFrameHex)},`);
    lines.push(`    errFrameHex: ${JSON.stringify(op.errorFixture.errFrameHex)},`);
    lines.push(`    messageUtf8: ${JSON.stringify(op.errorFixture.messageUtf8)},`);
    lines.push(`  },`);
    lines.push(`} as const;`);
    lines.push("");

    inputAliases.set(`${requestEvent.tsName}Input`, requestEvent.tsName);
    outputAliases.set(`${responseEvent.tsName}Output`, responseEvent.tsName);
  }

  // Type-alias-vs-event-name collision check: every alias name
  // (`<tsName>Input` / `<tsName>Output`) is a NEW declaration in
  // operations.ts. If the manifest also has an event whose tsName is
  // literally `FooInput` or `FooOutput`, the alias and the imported
  // event type clash (TS2440 duplicate identifier). Catch it here at
  // codegen time rather than letting tsc fail on the consumer side.
  const eventTsNames = new Set(manifest.events.map((e) => e.tsName));
  for (const [alias, target] of [...inputAliases, ...outputAliases]) {
    if (eventTsNames.has(alias) && alias !== target) {
      throw new CodegenError(
        "type_alias_event_name_collision",
        `operations.ts would emit \`export type ${alias} = ${target}\` but an event already declares the type ${alias}; rename the event or pick distinct operation input/output shapes`,
      );
    }
  }

  // Emit the deduped type aliases AFTER the operation consts. Two
  // operations sharing the same input event type now produce one
  // `export type FooInput = Foo` declaration, not two.
  if (inputAliases.size > 0 || outputAliases.size > 0) {
    lines.push(`// ─── shared event type aliases ──────────────────────`);
  }
  for (const [alias, target] of inputAliases) {
    lines.push(`export type ${alias} = ${target};`);
  }
  for (const [alias, target] of outputAliases) {
    lines.push(`export type ${alias} = ${target};`);
  }
  lines.push("");

  return lines.join("\n");
}

function renderIndexModule(): string {
  return [
    FILE_HEADER,
    `export * from "./events.js";`,
    `export * from "./operations.js";`,
    `export * from "./manifest.js";`,
    "",
  ].join("\n");
}

const SAFE_MIN = Number.MIN_SAFE_INTEGER;
const SAFE_MAX = Number.MAX_SAFE_INTEGER;

function schemaForToken(token: string): string {
  switch (token) {
    case "string":
      return "Schema.String";
    case "u8":
      return checkedNumber(0, 255);
    case "u16":
      return checkedNumber(0, 65535);
    case "u32":
      return checkedNumber(0, 4294967295);
    case "u64-safe":
    case "u64-millis":
      return checkedNumber(0, SAFE_MAX);
    case "u64-safe-positive":
      return checkedNumber(1, SAFE_MAX);
    case "i64-microseconds":
      return checkedNumber(SAFE_MIN, SAFE_MAX);
    case "option<string>":
      return "Schema.NullOr(Schema.String)";
    case "option<u128-hex>":
      return `Schema.NullOr(${brandedHex("EventIdHex", 32)})`;
    case "option<u8>":
      return `Schema.NullOr(${checkedNumber(0, 255)})`;
    case "option<u16>":
      return `Schema.NullOr(${checkedNumber(0, 65535)})`;
    case "option<u64-safe>":
      return `Schema.NullOr(${checkedNumber(0, SAFE_MAX)})`;
    case "bool":
      return "Schema.Boolean";
    case "map<string,string>":
      return "Schema.Record(Schema.String, Schema.String)";
    case "array<EventSummary>":
      return "Schema.Array(EventSummary)";
    case "u128-hex":
      return brandedHex("EventIdHex", 32);
    case "blake3-32-hex":
      return brandedHex("ContentHashHex", 64);
    case "ed25519-sig-hex":
      return brandedHex("SignatureHex", 128);
    case "key-id-hex":
      return brandedHex("KeyIdHex", 64);
    case "option<ed25519-sig-hex>":
      return `Schema.NullOr(${brandedHex("SignatureHex", 128)})`;
    case "option<blake3-32-hex>":
      return `Schema.NullOr(${brandedHex("ContentHashHex", 64)})`;
    case "hex-blob":
      return brandedHexBlob();
    case "map<string,hex-blob>":
      return `Schema.Record(Schema.String, ${brandedHexBlob()})`;
    default:
      throw new CodegenError(
        "unsupported_field_type",
        `internal: schemaForToken called with unknown token ${JSON.stringify(token)}`,
      );
  }
}

function checkedNumber(min: number, max: number): string {
  return `Schema.Number.pipe(Schema.check(Schema.isInt(), Schema.isBetween({ minimum: ${min}, maximum: ${max} })))`;
}

function brandedHex(brand: string, exactLength: number): string {
  // Lowercase-hex string of an exact length, branded so that passing a
  // wrong hex shape fails at the TS type checker.
  return `Schema.String.pipe(Schema.check(Schema.isPattern(/^[0-9a-f]{${exactLength}}$/u)), Schema.brand(${JSON.stringify(brand)}))`;
}

function brandedHexBlob(): string {
  return `Schema.String.pipe(Schema.check(Schema.isPattern(/^[0-9a-f]*$/u)), Schema.brand("HexBlob"))`;
}

function constCase(input: string): string {
  return input.replace(/([a-z0-9])([A-Z])/gu, "$1_$2").toUpperCase();
}

// Re-export the TS type generator for unit tests.
export function tsTypeForToken(token: string): string {
  switch (token) {
    case "string":
      return "string";
    case "u8":
    case "u16":
    case "u32":
    case "u64-safe":
    case "u64-safe-positive":
    case "u64-millis":
    case "i64-microseconds":
      return "number";
    case "option<string>":
      return "string | null";
    case "option<u128-hex>":
      return '(string & Schema.Brand<"EventIdHex">) | null';
    case "option<u8>":
    case "option<u16>":
    case "option<u64-safe>":
      return "number | null";
    case "bool":
      return "boolean";
    case "map<string,string>":
      return "Record<string, string>";
    case "array<EventSummary>":
      return "Array<EventSummary>";
    case "u128-hex":
      return 'string & Schema.Brand<"EventIdHex">';
    case "blake3-32-hex":
      return 'string & Schema.Brand<"ContentHashHex">';
    case "ed25519-sig-hex":
      return 'string & Schema.Brand<"SignatureHex">';
    case "key-id-hex":
      return 'string & Schema.Brand<"KeyIdHex">';
    case "option<ed25519-sig-hex>":
      return '(string & Schema.Brand<"SignatureHex">) | null';
    case "option<blake3-32-hex>":
      return '(string & Schema.Brand<"ContentHashHex">) | null';
    case "hex-blob":
      return 'string & Schema.Brand<"HexBlob">';
    case "map<string,hex-blob>":
      return 'Record<string, string & Schema.Brand<"HexBlob">>';
    default:
      throw new CodegenError(
        "unsupported_field_type",
        `internal: tsTypeForToken called with unknown token ${JSON.stringify(token)}`,
      );
  }
}
