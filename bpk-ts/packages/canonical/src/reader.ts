/**
 * Low-level byte reader for the canonical MessagePack decoder.
 *
 * Walks an immutable `Uint8Array` and exposes primitive read helpers
 * (`readByte`, `readBytes`, `readUInt16BE`, ...). The recursive
 * value-level decode logic lives in `./decode.ts`.
 */

export class CanonicalDecodeError extends Error {
  readonly code: string;
  constructor(code: string, message: string) {
    super(message);
    this.name = "CanonicalDecodeError";
    this.code = code;
  }
}

export class Reader {
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
    return (a * 0x1000000 + (b << 16) + (c << 8) + d) >>> 0;
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
