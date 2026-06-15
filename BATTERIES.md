# Batteries

Free Battery Factory is the workshop. Each battery is a bounded component that stores, runs, exposes, or checks part of a software boundary.

## Shipped Batteries

| Battery | Role |
| --- | --- |
| `batpak` | Core battery pack format and embedded event substrate. |
| `syncbat` | Sync-first runtime contracts and dispatch surfaces. |
| `netbat` | Server and network boundary surfaces for explicit IO. |
| `hbat` | Live operation handling surface in the current workspace. |
| `@batpak/sdk` / [bpk-ts/](bpk-ts/README.md) | NETBAT/1 wire client, canonical codec, manifest-generated types. |
| `batpak-macros` | Derive macro support for the core substrate. |
| `syncbat-macros` | Derive macro support for syncbat. |
| `batpak-bench-support` | Shared benchmark support for workspace surfaces. |

## Support Crates

`batpak-macros-support`, `tools/integrity`, and `tools/xtask` are factory machinery. They support the batteries, but they are not application batteries themselves.

## Reserved Names

The following names are reserved vocabulary only. They are not shipped products unless a future release adds crates, tests, traceability, and conformance surfaces for them:

- `wirebat`
- `filebat`
- `testbat`
- `benchbat`
- `viewbat`
- `guardbat`
- `shipbat`

## Usage Rhythm

Need local event truth? Use `batpak`.

Need sync-first runtime behavior? Use `syncbat`.

Need explicit network wiring? Use `netbat`.

Need a live reference host? Use `hbat` and prove it with `just host-dev`.

Need TypeScript against that host? Use `@batpak/sdk` — see [bpk-ts/README.md](bpk-ts/README.md).

Need conformance or release checks? Use `just inspect`, `just verify`, and `just seal`.

