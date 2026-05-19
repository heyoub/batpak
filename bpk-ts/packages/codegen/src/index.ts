/**
 * BatPAK TypeScript codegen.
 *
 * Reads `batpak.manifest.json` and emits TypeScript symbols into a
 * fresh `packages/generated/src` tree. Output is FULLY OVERWRITTEN on
 * each run; the codegen never patches existing files.
 *
 * Phase 0 supports the BatPAK TS manifest at `manifestVersion: 1`.
 * Other versions are refused with a clear error message.
 *
 * The plan-locked Phase 0 wire-token vocabulary:
 *   - "string"      -> TypeScript `string`
 *   - "u64-millis"  -> TypeScript `number` (safe-integer bounded)
 *
 * Anything outside that vocabulary is refused so future expansions
 * become deliberate.
 */

import { mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";

export const SUPPORTED_MANIFEST_VERSION = 1;

const SUPPORTED_FIELD_TYPES = new Set<string>(["string", "u64-millis"]);

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
  if (typeof value !== "object" || value === null) {
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
          `${source}: ${event.name}.${field.wireName} uses unsupported typeToken ${JSON.stringify(field.typeToken)}; Phase 0 supports ${[...SUPPORTED_FIELD_TYPES].join(", ")}`,
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
  // Full overwrite: codegen never patches an existing tree.
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

function renderManifestModule(
  manifest: BatpakTsManifest,
  options: GenerateOptions,
): string {
  const json = JSON.stringify(manifest, null, 2);
  return [
    FILE_HEADER,
    `// Source manifest: ${options.manifestPath}`,
    `export const MANIFEST_VERSION = ${manifest.manifestVersion} as const;`,
    `export const NETBAT_VERSION = ${JSON.stringify(manifest.netbatVersion)} as const;`,
    `export const BATPAK_VERSION = ${JSON.stringify(manifest.batpakVersion)} as const;`,
    `export const CANONICAL_ENCODING = ${JSON.stringify(manifest.canonicalEncoding)} as const;`,
    `export const BATPAK_TS_MANIFEST = ${json} as const;`,
    "",
  ].join("\n");
}

function renderEventsModule(manifest: BatpakTsManifest): string {
  const lines: string[] = [FILE_HEADER];
  for (const event of manifest.events) {
    lines.push(`/** Source: ${event.rustType}; category=${event.category}, typeId=${event.typeId} */`);
    lines.push(`export interface ${event.tsName} {`);
    for (const field of event.fields) {
      lines.push(`  ${field.wireName}: ${tsTypeForToken(field.typeToken)};`);
    }
    lines.push(`}`);
    lines.push("");
    const constSafeName = constCase(event.tsName);
    lines.push(
      `export const ${constSafeName}_GOLDEN_HEX = ${JSON.stringify(event.goldenPayloadHex)} as const;`,
    );
    lines.push(
      `export const ${constSafeName}_FIXTURE: ${event.tsName} = ${JSON.stringify(event.fixtureValue, null, 2)};`,
    );
    lines.push("");
  }
  return lines.join("\n");
}

function renderOperationsModule(manifest: BatpakTsManifest): string {
  const lines: string[] = [FILE_HEADER];
  lines.push(`import type { ${manifest.events.map((e) => e.tsName).join(", ")} } from "./events.js";`);
  lines.push("");
  for (const op of manifest.operations) {
    const requestEvent = manifest.events.find((e) => e.name === op.inputEvent);
    const responseEvent = manifest.events.find((e) => e.name === op.outputEvent);
    if (!requestEvent || !responseEvent) {
      throw new CodegenError(
        "invalid_manifest_shape",
        `operation ${op.name}: missing referenced event (${op.inputEvent} or ${op.outputEvent})`,
      );
    }
    const constName = constCase(op.name).replace(/\./gu, "_").toUpperCase();
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
    lines.push(``);
    lines.push(`export type ${requestEvent.tsName}Input = ${requestEvent.tsName};`);
    lines.push(`export type ${responseEvent.tsName}Output = ${responseEvent.tsName};`);
    lines.push("");
  }
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

function tsTypeForToken(token: string): string {
  switch (token) {
    case "string":
      return "string";
    case "u64-millis":
      // Safe-JS-integer-bounded; parity tests assert <= Number.MAX_SAFE_INTEGER.
      return "number";
    default:
      throw new CodegenError(
        "unsupported_field_type",
        `internal: tsTypeForToken called with unknown token ${JSON.stringify(token)}`,
      );
  }
}

function constCase(input: string): string {
  return input.replace(/([a-z0-9])([A-Z])/gu, "$1_$2").toUpperCase();
}
