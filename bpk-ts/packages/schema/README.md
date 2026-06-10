# @batpak/schema

Effect 4 Schema bridge for [batpak](https://github.com/heyoub/batpak):
`decodeBytes`/`encodeBytes` wrap the canonical codec with runtime validation,
and `bank.event()` is the authoring API for downstream-only TypeScript event
schemas.

Most applications should install [`@batpak/sdk`](https://www.npmjs.com/package/@batpak/sdk),
which re-exports this package.

## License

MIT OR Apache-2.0.
