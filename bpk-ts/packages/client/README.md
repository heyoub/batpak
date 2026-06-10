# @batpak/client

NETBAT/1 wire client for [batpak](https://github.com/heyoub/batpak) hosts:
frame writer/reader, a typed error union covering all 12 stable
`NetbatError` codes, and TCP transport via a duck-typed socket interface.

Most applications should install [`@batpak/sdk`](https://www.npmjs.com/package/@batpak/sdk),
which re-exports this package. Install `@batpak/client` directly only when
you want the wire client without the codec, schema, or generated types.

## License

MIT OR Apache-2.0.
