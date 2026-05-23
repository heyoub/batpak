#!/usr/bin/env node
// AUTO-LOADER: compiled `dist/` if it exists; falls back to tsx for dev.
import { existsSync } from "node:fs";
import { fileURLToPath, pathToFileURL } from "node:url";
import { dirname, resolve } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
const compiled = resolve(here, "../dist/cli.js");
if (existsSync(compiled)) {
  await import(pathToFileURL(compiled).href);
} else {
  const srcCli = resolve(here, "../src/cli.ts");
  // Prefer tsx if available; otherwise fall back to node --experimental-strip-types
  // which Node 22+ supports for plain .ts files.
  try {
    await import("tsx/esm");
    await import(pathToFileURL(srcCli).href);
  } catch {
    // Node 22 type stripping needs --experimental-strip-types passed on
    // the CLI, so we cannot dynamically import .ts here without it. In
    // CI we always run after `pnpm -r build`, so the compiled path
    // exists. Local dev should run `pnpm -w build` first.
    console.error(
      "batpak-codegen: compiled dist/cli.js not found and tsx is not installed. Run `pnpm -w build` first.",
    );
    process.exit(2);
  }
}
