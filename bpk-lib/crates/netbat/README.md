# netbat

Thin sync-first exposure boundary for syncbat.

```text
nb exposes.
```

`netbat` owns route metadata, bounded request/response frames, stable error-code
mapping, and blocking transport helpers. Runtime dispatch stays in `syncbat`;
durable records, receipts, evidence, and delivery witnesses stay in `batpak`.

The request/response boundary contract is documented in repository ADR-0029.
The streaming contract shape is documented in repository ADR-0030.

