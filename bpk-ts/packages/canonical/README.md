# @batpak/canonical

Named-field MessagePack codec for [batpak](https://github.com/heyoub/batpak),
byte-for-byte identical to the Rust `rmp-serde 1.3.1` encoder and
parity-tested against it in CI. Fields follow Rust struct declaration order.

Most applications should install [`@batpak/sdk`](https://www.npmjs.com/package/@batpak/sdk),
which re-exports this package. Install `@batpak/canonical` directly only when
you want the codec without the client, schema, or generated types.

## License

MIT OR Apache-2.0.
