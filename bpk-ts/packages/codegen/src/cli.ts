import { resolve } from "node:path";
import { generate, CodegenError } from "./index.js";

function parseArgs(argv: readonly string[]): { manifest: string; out: string } {
  let manifest: string | null = null;
  let out: string | null = null;
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === "--manifest") {
      manifest = argv[i + 1] ?? null;
      i += 1;
    } else if (arg === "--out") {
      out = argv[i + 1] ?? null;
      i += 1;
    } else if (arg === "--help" || arg === "-h") {
      printUsage();
      process.exit(0);
    } else {
      console.error(`batpak-codegen: unknown argument ${JSON.stringify(arg)}`);
      printUsage();
      process.exit(2);
    }
  }
  if (manifest === null || out === null) {
    console.error("batpak-codegen: --manifest and --out are required");
    printUsage();
    process.exit(2);
  }
  return { manifest, out };
}

function printUsage(): void {
  console.error(
    "Usage: batpak-codegen --manifest <path/to/batpak.manifest.json> --out <output-dir>",
  );
}

function main(): void {
  const { manifest, out } = parseArgs(process.argv.slice(2));
  try {
    generate({
      manifestPath: resolve(manifest),
      outDir: resolve(out),
    });
    console.error(`batpak-codegen: wrote ${out}`);
  } catch (error) {
    if (error instanceof CodegenError) {
      console.error(`batpak-codegen: ${error.code}: ${error.message}`);
      process.exit(2);
    }
    throw error;
  }
}

main();
