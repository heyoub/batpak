# Vocabulary

This file names the live vocabulary target for the `0.7.6` pre-1.0
correction cut. It is intentionally small: use it when deciding whether a
public name should stay, move behind an accessor, or become internal.

## Boundary Rule

Keep engine names in engine APIs and userland names in userland facades.

- `Store`, `GateSet`, `Pipeline`, `Cursor`, `Subscription`, `FrontierView`,
  and `EventKind` are engine-facing names.
- `Bank`, `Register`, `Checkout`, `Pass`, `Receipt`, `Claim`, and
  `Statement` are userland facade names.

Do not rename substrate internals to userland nouns. Do not expose basement
machinery as the default userland surface.

## Canonical Terms

| Term | Use For | Avoid |
| --- | --- | --- |
| `Coordinate` | public `(entity, scope)` address | `coord` in public fields |
| `EventKind` | packed category/type event discriminator | bare `kind` in docs without context |
| `position` | event position, including lane/depth hints | overloaded `pos` |
| `DiskPos` | low-level physical receipt witness | prelude/default user import |
| `frontier` | coherent public operator view of watermarks | public raw watermark snapshots |
| `FrontierView` | public frontier struct | duplicate public snapshot structs |
| `WatermarkSnapshot` | internal raw watermark machinery | public operator surface |
| `Cursor` | ordered pull replay, checkpointable | generic "stream" wording |
| `Subscription` | lossy push observation | guaranteed delivery wording |
| `Canal` | delivery adapter concept over cursor/subscription primitives | metaphor-only docs |
| `by_entity` | exact entity query returning indexed entries | `stream` as a public query name |
| `read_raw` | raw event read by id | `get_raw` |
| `Ctx` | runtime context spelling | `Cx` in new public surfaces |

## Public Surface Corrections

The `0.7.6` cut may break pre-1.0 API when the old surface advertises the
wrong shape. The intended corrections are:

- hide internal helpers such as `ClockKey`
- remove low-level witnesses such as `DiskPos` from default imports
- make `StoreConfig` construction flow through constructors/builders instead
  of public mutable fields
- keep one public frontier view and move raw watermark snapshots behind the
  internal/test boundary
- remove old aliases after the canonical names have landed

## Delivery Words

The store has two shipped delivery primitives:

- `Subscription`: lossy push observation. It favors writer isolation and may
  drop events under backpressure.
- `Cursor`: ordered pull replay. It favors completeness and may checkpoint for
  durable at-least-once replay.

`Canal` is the shared delivery adapter vocabulary over those primitives. It
must stay narrow and compositional: the primitive keeps its own semantics, and
the canal layer only gives reactor code a common way to consume committed work.

