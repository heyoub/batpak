# refbat

Reference NETBAT/1 host for the batpak family (`publish = false`).

`refbat` registers all ten manifest operations against a real `Store`: five core
substrate terminals plus `system.heartbeat` and the four domain-neutral
`evidence.*` ops. Runtime dispatch stays in `syncbat`; wire framing stays in
`netbat`; durable records, receipts, and evidence stay in `batpak`.

```text
hb hosts.
```

## Proof

```sh
just host-dev
```

From the repository root — exports the manifest, builds the workspace, boots
`refbat` on an ephemeral store, and runs the TypeScript heartbeat-spike through
commit, query, and get.

## Boot (manual)

```sh
cargo run -p refbat -- serve \
  --store "$(mktemp -d)" \
  --tcp 127.0.0.1:0 \
  --print-port
```

The first stdout line is machine-readable rendezvous JSON (`REFBAT_READY …`).

## Docs

- Terminals: [TERMINALS.md](../../../TERMINALS.md)
- Integration and host loops: [INTEGRATION.md](../../../INTEGRATION.md)
- TypeScript clients: [bpk-ts/README.md](../../../bpk-ts/README.md)
