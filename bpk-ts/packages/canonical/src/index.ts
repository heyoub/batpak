/**
 * Canonical named-field MessagePack codec for the BatPAK TS SDK.
 *
 * The output of {@link encode} MUST equal the bytes produced by the Rust
 * side's `batpak::encoding::to_bytes` (which wraps
 * `rmp_serde::to_vec_named`) for the Phase 0 fixture subset:
 *   - JSON-compatible plain object whose keys are strings.
 *   - Field iteration order is the object's own insertion order
 *     (the Rust side iterates serde struct field declaration order).
 *   - Values are: string, finite non-negative integer (<= 2^53 - 1),
 *     boolean, null, nested object, or array.
 *
 * No floats, no Date, no Map/Set, no BigInt — these are deliberately
 * excluded for Phase 0 and would silently produce wrong bytes if added.
 *
 * Encoders for variable widths pick the shortest valid MessagePack
 * encoding to match rmp-serde's `write_uint` / `write_str` shortest-
 * encoding rule.
 *
 * This module is a barrel — implementation lives in:
 *   - `./hex.ts`     — `encodeHex`, `decodeHex`
 *   - `./writer.ts`  — `Writer`, `CanonicalEncodeError`
 *   - `./reader.ts`  — `Reader`, `CanonicalDecodeError`
 *   - `./encode.ts`  — `encode`, `EncodeOptions`, `DEFAULT_MAX_BYTES`
 *   - `./decode.ts`  — `decode`
 */

export { decodeHex, encodeHex } from "./hex.js";
export { CanonicalEncodeError } from "./writer.js";
export { CanonicalDecodeError } from "./reader.js";
export { DEFAULT_MAX_BYTES, encode, type EncodeOptions } from "./encode.js";
export { decode } from "./decode.js";
