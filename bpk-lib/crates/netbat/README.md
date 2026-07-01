# netbat

A lean, sync-first exposure boundary for syncbat: a blocking transport layer with opt-in TLS.

```text
nb exposes.
```

`netbat` owns route metadata, bounded request/response frames, stable error-code
mapping, and blocking transport helpers. Runtime dispatch stays in `syncbat`;
durable records, receipts, evidence, and delivery witnesses stay in `batpak`.

The request/response boundary contract is documented in repository ADR-0029.
The streaming contract shape is documented in repository ADR-0030.

## Connections and dispatch

Listeners cap connections with `ConnectionLimit`. The default,
`ConnectionLimit::Concurrent`, is an in-flight permit pool (at most `n`
connections served at once, a freed slot immediately reusable). It replaces the
pre-0.9 `max_connections` lifetime accept budget, now the explicit opt-in
`ConnectionLimit::Lifetime`; `ConnectionLimit::Unlimited` removes the gate. The
subscription listener serves sessions concurrently by default
(`SubscriptionDispatch::Concurrent`, one contained worker per session);
`SubscriptionDispatch::Sequential` keeps the pre-0.9 inline one-at-a-time path.

## Security / transport trust model

netbat has **no authentication and no authorization, by design**. Identity and
access control are downstream-domain concerns: authenticate at a fronting proxy
or in the application layer that owns the runtime, never inside netbat.

Without the `tls` feature (the default) netbat speaks plaintext and assumes a
**trusted transport** — bind it to loopback, a private segment, or behind a
TLS-terminating reverse proxy. The opt-in `tls` feature adds server-only TLS
(rustls): confidentiality and server identity only, never client auth. Build a
`TlsServerConfig` from PEM and pass `TransportSecurity::Tls` to
`serve_tcp_listener_secured`; the handshake runs per-connection post-permit, and
a failed handshake is counted in `tls_handshake_failures` and dropped, never
listener-fatal.

Live terminals: [05_TERMINALS.md](../../../05_TERMINALS.md). Composition:
[11_INTEGRATION.md](../../../11_INTEGRATION.md).
