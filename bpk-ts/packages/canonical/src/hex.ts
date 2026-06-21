/**
 * Lowercase-hex helpers used by NETBAT framing.
 *
 * These are the lowest-level primitives in the canonical package and
 * are independent of MessagePack encoding/decoding.
 */

import { CanonicalDecodeError } from "./reader.js";

/**
 * Lowercase-hex encode a byte string.
 *
 * Matches the Rust side's `netbat::transport::encode_hex_into` (and the
 * refbat manifest `encode_hex` helper) byte-for-byte.
 *
 * @example
 * ```ts
 * import { encodeHex } from "@batpak/canonical";
 *
 * encodeHex(new Uint8Array([0xde, 0xad, 0xbe, 0xef]));
 * // => "deadbeef"
 * ```
 */
export function encodeHex(bytes: Uint8Array): string {
  const HEX = "0123456789abcdef";
  let out = "";
  for (const byte of bytes) {
    out += HEX[(byte >> 4) & 0x0f];
    out += HEX[byte & 0x0f];
  }
  return out;
}

/**
 * Decode a lowercase-or-mixed-case hex string back to bytes. Matches the
 * Rust decoder's permissive case behavior.
 *
 * @throws {CanonicalDecodeError} when the input has odd length or
 * contains a non-hex character.
 *
 * @example
 * ```ts
 * import { decodeHex } from "@batpak/canonical";
 *
 * Array.from(decodeHex("DEADbeef"));
 * // => [0xde, 0xad, 0xbe, 0xef]
 * ```
 */
export function decodeHex(hex: string): Uint8Array {
  if (hex.length % 2 !== 0) {
    throw new CanonicalDecodeError(
      "odd_hex_length",
      `hex string of length ${hex.length} is not byte-aligned`,
    );
  }
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < hex.length; i += 2) {
    const high = hexCharValue(hex.charCodeAt(i));
    const low = hexCharValue(hex.charCodeAt(i + 1));
    out[i / 2] = (high << 4) | low;
  }
  return out;
}

function hexCharValue(code: number): number {
  if (code >= 48 && code <= 57) return code - 48;
  if (code >= 97 && code <= 102) return code - 97 + 10;
  if (code >= 65 && code <= 70) return code - 65 + 10;
  throw new CanonicalDecodeError("non_hex_char", `non-hex character at code point ${code}`);
}
