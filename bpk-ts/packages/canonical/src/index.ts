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
 */

export interface EncodeOptions {
  /**
   * Maximum bytes allowed in the encoded output. Defaults to 32 KiB,
   * matching netbat's `DEFAULT_MAX_INPUT_BYTES`.
   */
  readonly maxBytes?: number;
}

export const DEFAULT_MAX_BYTES = 32 * 1024;

export class CanonicalEncodeError extends Error {
  readonly code: string;
  constructor(code: string, message: string) {
    super(message);
    this.name = "CanonicalEncodeError";
    this.code = code;
  }
}

export class CanonicalDecodeError extends Error {
  readonly code: string;
  constructor(code: string, message: string) {
    super(message);
    this.name = "CanonicalDecodeError";
    this.code = code;
  }
}

/**
 * Encode a JSON-shaped value into canonical named-field MessagePack
 * bytes.
 */
export function encode(value: unknown, options: EncodeOptions = {}): Uint8Array {
  const max = options.maxBytes ?? DEFAULT_MAX_BYTES;
  const writer = new Writer();
  encodeValue(writer, value);
  if (writer.length > max) {
    throw new CanonicalEncodeError(
      "output_too_large",
      `encoded length ${writer.length} exceeds max ${max}`,
    );
  }
  return writer.toUint8Array();
}

/**
 * Decode canonical named-field MessagePack bytes back into a
 * JSON-shaped value.
 */
export function decode(bytes: Uint8Array): unknown {
  const reader = new Reader(bytes);
  const value = decodeValue(reader);
  if (!reader.atEnd()) {
    throw new CanonicalDecodeError(
      "trailing_bytes",
      `decoder finished at offset ${reader.offset} but buffer has ${bytes.length} bytes`,
    );
  }
  return value;
}

const POS_FIXINT_MAX = 0x7f;
const FIXMAP_MAX = 0x0f;
const FIXSTR_MAX = 0x1f;
const FIXARRAY_MAX = 0x0f;

const NIL = 0xc0;
const FALSE = 0xc2;
const TRUE = 0xc3;
const UINT8 = 0xcc;
const UINT16 = 0xcd;
const UINT32 = 0xce;
const UINT64 = 0xcf;
const STR8 = 0xd9;
const STR16 = 0xda;
const STR32 = 0xdb;
const ARRAY16 = 0xdc;
const ARRAY32 = 0xdd;
const MAP16 = 0xde;
const MAP32 = 0xdf;

class Writer {
  private chunks: number[] = [];

  get length(): number {
    return this.chunks.length;
  }

  pushByte(byte: number): void {
    this.chunks.push(byte & 0xff);
  }

  pushBytes(bytes: Uint8Array): void {
    for (const byte of bytes) {
      this.chunks.push(byte);
    }
  }

  toUint8Array(): Uint8Array {
    return new Uint8Array(this.chunks);
  }
}

function encodeValue(writer: Writer, value: unknown): void {
  if (value === null) {
    writer.pushByte(NIL);
    return;
  }
  if (value === true) {
    writer.pushByte(TRUE);
    return;
  }
  if (value === false) {
    writer.pushByte(FALSE);
    return;
  }
  if (typeof value === "string") {
    encodeString(writer, value);
    return;
  }
  if (typeof value === "number") {
    encodeNumber(writer, value);
    return;
  }
  if (Array.isArray(value)) {
    encodeArray(writer, value);
    return;
  }
  if (typeof value === "object") {
    encodeObject(writer, value as Record<string, unknown>);
    return;
  }
  throw new CanonicalEncodeError(
    "unsupported_value",
    `Phase 0 canonical encoder rejects value of type ${typeof value}`,
  );
}

function encodeString(writer: Writer, value: string): void {
  const utf8 = new TextEncoder().encode(value);
  const len = utf8.length;
  if (len <= FIXSTR_MAX) {
    writer.pushByte(0xa0 | len);
  } else if (len <= 0xff) {
    writer.pushByte(STR8);
    writer.pushByte(len);
  } else if (len <= 0xffff) {
    writer.pushByte(STR16);
    writer.pushByte((len >> 8) & 0xff);
    writer.pushByte(len & 0xff);
  } else if (len <= 0xffffffff) {
    writer.pushByte(STR32);
    writer.pushByte((len >>> 24) & 0xff);
    writer.pushByte((len >>> 16) & 0xff);
    writer.pushByte((len >>> 8) & 0xff);
    writer.pushByte(len & 0xff);
  } else {
    throw new CanonicalEncodeError(
      "string_too_long",
      `string of length ${len} bytes exceeds MessagePack str32 limit`,
    );
  }
  writer.pushBytes(utf8);
}

function encodeNumber(writer: Writer, value: number): void {
  if (!Number.isInteger(value)) {
    throw new CanonicalEncodeError(
      "unsupported_value",
      `Phase 0 canonical encoder rejects non-integer number ${value}`,
    );
  }
  if (value < 0) {
    throw new CanonicalEncodeError(
      "unsupported_value",
      `Phase 0 canonical encoder rejects negative integer ${value} (no signed payload fields in fixtures)`,
    );
  }
  if (!Number.isSafeInteger(value)) {
    throw new CanonicalEncodeError(
      "unsupported_value",
      `Phase 0 canonical encoder rejects integer ${value} above Number.MAX_SAFE_INTEGER`,
    );
  }
  encodeUnsignedInt(writer, value);
}

function encodeUnsignedInt(writer: Writer, value: number): void {
  if (value <= POS_FIXINT_MAX) {
    writer.pushByte(value);
    return;
  }
  if (value <= 0xff) {
    writer.pushByte(UINT8);
    writer.pushByte(value);
    return;
  }
  if (value <= 0xffff) {
    writer.pushByte(UINT16);
    writer.pushByte((value >> 8) & 0xff);
    writer.pushByte(value & 0xff);
    return;
  }
  if (value <= 0xffffffff) {
    writer.pushByte(UINT32);
    writer.pushByte((value >>> 24) & 0xff);
    writer.pushByte((value >>> 16) & 0xff);
    writer.pushByte((value >>> 8) & 0xff);
    writer.pushByte(value & 0xff);
    return;
  }
  // u64 big-endian. JavaScript Number can losslessly hold integers up to
  // 2^53 - 1, which fits in 53 bits — pack as 8-byte big-endian.
  writer.pushByte(UINT64);
  const high = Math.floor(value / 0x100000000);
  const low = value >>> 0;
  writer.pushByte((high >>> 24) & 0xff);
  writer.pushByte((high >>> 16) & 0xff);
  writer.pushByte((high >>> 8) & 0xff);
  writer.pushByte(high & 0xff);
  writer.pushByte((low >>> 24) & 0xff);
  writer.pushByte((low >>> 16) & 0xff);
  writer.pushByte((low >>> 8) & 0xff);
  writer.pushByte(low & 0xff);
}

function encodeArray(writer: Writer, value: unknown[]): void {
  const len = value.length;
  if (len <= FIXARRAY_MAX) {
    writer.pushByte(0x90 | len);
  } else if (len <= 0xffff) {
    writer.pushByte(ARRAY16);
    writer.pushByte((len >> 8) & 0xff);
    writer.pushByte(len & 0xff);
  } else {
    writer.pushByte(ARRAY32);
    writer.pushByte((len >>> 24) & 0xff);
    writer.pushByte((len >>> 16) & 0xff);
    writer.pushByte((len >>> 8) & 0xff);
    writer.pushByte(len & 0xff);
  }
  for (const item of value) {
    encodeValue(writer, item);
  }
}

function encodeObject(writer: Writer, value: Record<string, unknown>): void {
  const keys = Object.keys(value);
  const len = keys.length;
  if (len <= FIXMAP_MAX) {
    writer.pushByte(0x80 | len);
  } else if (len <= 0xffff) {
    writer.pushByte(MAP16);
    writer.pushByte((len >> 8) & 0xff);
    writer.pushByte(len & 0xff);
  } else {
    writer.pushByte(MAP32);
    writer.pushByte((len >>> 24) & 0xff);
    writer.pushByte((len >>> 16) & 0xff);
    writer.pushByte((len >>> 8) & 0xff);
    writer.pushByte(len & 0xff);
  }
  for (const key of keys) {
    encodeString(writer, key);
    encodeValue(writer, value[key]);
  }
}

class Reader {
  readonly bytes: Uint8Array;
  offset = 0;

  constructor(bytes: Uint8Array) {
    this.bytes = bytes;
  }

  atEnd(): boolean {
    return this.offset >= this.bytes.length;
  }

  readByte(): number {
    if (this.offset >= this.bytes.length) {
      throw new CanonicalDecodeError("eof", "unexpected end of input");
    }
    const byte = this.bytes[this.offset];
    this.offset += 1;
    if (byte === undefined) {
      throw new CanonicalDecodeError("eof", "unexpected end of input");
    }
    return byte;
  }

  readBytes(n: number): Uint8Array {
    if (this.offset + n > this.bytes.length) {
      throw new CanonicalDecodeError(
        "eof",
        `expected ${n} bytes at offset ${this.offset}, have ${this.bytes.length - this.offset}`,
      );
    }
    const out = this.bytes.subarray(this.offset, this.offset + n);
    this.offset += n;
    return out;
  }

  readUInt16BE(): number {
    const a = this.readByte();
    const b = this.readByte();
    return (a << 8) | b;
  }

  readUInt32BE(): number {
    const a = this.readByte();
    const b = this.readByte();
    const c = this.readByte();
    const d = this.readByte();
    return ((a * 0x1000000) + (b << 16) + (c << 8) + d) >>> 0;
  }

  readUInt64BE(): number {
    const high = this.readUInt32BE();
    const low = this.readUInt32BE();
    const combined = high * 0x100000000 + low;
    if (!Number.isSafeInteger(combined)) {
      throw new CanonicalDecodeError(
        "unsafe_integer",
        `u64 value ${high}*2^32+${low} exceeds Number.MAX_SAFE_INTEGER; Phase 0 fixtures must stay in safe range`,
      );
    }
    return combined;
  }
}

function decodeValue(reader: Reader): unknown {
  const head = reader.readByte();
  // Positive fixint
  if (head <= POS_FIXINT_MAX) {
    return head;
  }
  // fixmap
  if (head >= 0x80 && head <= 0x8f) {
    return decodeMap(reader, head & 0x0f);
  }
  // fixarray
  if (head >= 0x90 && head <= 0x9f) {
    return decodeArray(reader, head & 0x0f);
  }
  // fixstr
  if (head >= 0xa0 && head <= 0xbf) {
    return decodeString(reader, head & 0x1f);
  }
  switch (head) {
    case NIL:
      return null;
    case FALSE:
      return false;
    case TRUE:
      return true;
    case UINT8:
      return reader.readByte();
    case UINT16:
      return reader.readUInt16BE();
    case UINT32:
      return reader.readUInt32BE();
    case UINT64:
      return reader.readUInt64BE();
    case STR8:
      return decodeString(reader, reader.readByte());
    case STR16:
      return decodeString(reader, reader.readUInt16BE());
    case STR32:
      return decodeString(reader, reader.readUInt32BE());
    case ARRAY16:
      return decodeArray(reader, reader.readUInt16BE());
    case ARRAY32:
      return decodeArray(reader, reader.readUInt32BE());
    case MAP16:
      return decodeMap(reader, reader.readUInt16BE());
    case MAP32:
      return decodeMap(reader, reader.readUInt32BE());
    default:
      throw new CanonicalDecodeError(
        "unsupported_token",
        `Phase 0 canonical decoder does not handle MessagePack token 0x${head.toString(16).padStart(2, "0")}`,
      );
  }
}

function decodeString(reader: Reader, length: number): string {
  const bytes = reader.readBytes(length);
  return new TextDecoder("utf-8", { fatal: true }).decode(bytes);
}

function decodeArray(reader: Reader, length: number): unknown[] {
  const out: unknown[] = [];
  for (let i = 0; i < length; i += 1) {
    out.push(decodeValue(reader));
  }
  return out;
}

function decodeMap(reader: Reader, length: number): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (let i = 0; i < length; i += 1) {
    const key = decodeValue(reader);
    if (typeof key !== "string") {
      throw new CanonicalDecodeError(
        "non_string_key",
        `Phase 0 canonical decoder requires string map keys (saw ${typeof key})`,
      );
    }
    out[key] = decodeValue(reader);
  }
  return out;
}

/**
 * Lowercase-hex encode a byte string.
 *
 * Matches the Rust side's `netbat::transport::encode_hex_into` (and the
 * hbat manifest `encode_hex` helper) byte-for-byte.
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
  throw new CanonicalDecodeError(
    "non_hex_char",
    `non-hex character at code point ${code}`,
  );
}
