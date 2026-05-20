/**
 * Low-level byte writer for the canonical MessagePack encoder.
 *
 * Owns the growing buffer and exposes minimal byte/byte-array push
 * primitives. Kept deliberately tiny so the encode logic in
 * `./encode.ts` stays the single source of truth for tag/length rules.
 */

export class CanonicalEncodeError extends Error {
  readonly code: string;
  constructor(code: string, message: string) {
    super(message);
    this.name = "CanonicalEncodeError";
    this.code = code;
  }
}

export class Writer {
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
