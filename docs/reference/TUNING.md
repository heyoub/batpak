# Tuning Guide

All settings live in `StoreConfig`. Create one via `StoreConfig::new(data_dir)`,
use the fluent `with_*` helpers for the knobs you care about, then pass it to
`Store::open()` or one of the cache convenience constructors.

`StoreConfig` groups related settings into four sub-structs: `BatchConfig`,
`WriterConfig`, `SyncConfig`, and `IndexConfig`. The `with_*` builder methods
abstract over the nested layout — you don't need to construct sub-structs by
hand unless you want struct-literal initialization.

## Configuration Reference

| Field path | Default | Unit | When to change |
|------------|---------|------|----------------|
| `segment_max_bytes` | 256 MB | bytes | Lower for embedded (e.g. 16 MB). Higher if events are large and you want fewer files. |
| `sync.every_n_events` | 1000 | events | Lower (1-10) for strict durability. Higher (5000+) for throughput-first workloads. |
| `fd_budget` | 64 | count | Raise if you have many segments open concurrently. Lower on FD-constrained systems. |
| `writer.channel_capacity` | 4096 | messages | Back-pressure threshold. Raise if producers are bursty. Lower to surface pressure earlier. |
| `broadcast_capacity` | 8192 | messages | Per-subscriber lossy ring. Raise for slow consumers. Lower to bound memory per subscriber. |
| `writer.restart_policy` | `RestartPolicy::default()` | - | Controls writer thread recovery from panics. |
| `writer.shutdown_drain_limit` | 1024 | messages | Max queued commands drained on shutdown. Raise if shutdown drops events. |
| `writer.stack_size` | `None` (OS default) | bytes | Set explicitly if writer thread needs deep stacks (recursive projections). |
| `clock` | `None` (SystemTime) | - | Inject deterministic clock for testing. Returns microseconds since epoch. |
| `sync.mode` | `SyncAll` | - | `SyncData` skips metadata fsync (faster, slightly less crash-safe). |
| `batch.group_commit_max_batch` | 1 | count | 0 = unbounded drain (batch all pending). >1 = batch N appends per fsync. **Requires idempotency keys** on every append when >1. |
| `batch.max_size` | 256 | count | Maximum items per `append_batch` call. |
| `batch.max_bytes` | 1 MB | bytes | Maximum total payload bytes per `append_batch` call. |
| `index.layout` | `AoS` | enum | `SoA` for scan-heavy, `AoSoA8/16/64` for SIMD, `SoAoS` for entity-local. Replaces DashMap scan indexes. |
| `index.incremental_projection` | `false` | bool | Enable for types with `supports_incremental_apply()=true`. Applies only delta events to cached state. |
| `index.enable_checkpoint` | `true` | bool | Writes `index.ckpt` on close for fast cold start. Disable for ephemeral test stores. |

## Tradeoff Matrix

### Durability vs Throughput
- `sync.every_n_events = 1` + `sync.mode = SyncAll` = maximum durability, lowest throughput
- `sync.every_n_events = 5000` + `sync.mode = SyncData` = maximum throughput, window of loss on crash

### Memory vs File Handles
- High `fd_budget` = more segments memory-mapped = faster reads, more FDs
- Low `fd_budget` = more LRU eviction = fewer FDs, occasional re-open cost

### Back-pressure vs Latency
- High `writer.channel_capacity` = producers rarely block, higher peak memory
- Low `writer.channel_capacity` = producers block early, bounded memory, higher tail latency

## Example: Embedded Device

```rust
let config = StoreConfig::new("/data/events")
    .with_segment_max_bytes(16 * 1024 * 1024)
    .with_fd_budget(8)
    .with_sync_every_n_events(10)
    .with_writer_channel_capacity(256);
```

## Example: High-Throughput Server

```rust
let config = StoreConfig::new("/var/lib/events")
    .with_segment_max_bytes(1024 * 1024 * 1024)
    .with_sync_every_n_events(5000)
    .with_sync_mode(SyncMode::SyncData)
    .with_group_commit_max_batch(64)
    .with_fd_budget(256)
    .with_writer_channel_capacity(16384)
    .with_broadcast_capacity(32768);
// NOTE: batch.group_commit_max_batch > 1 requires idempotency keys on every append.
```

## Example: ECS / Analytical Workload

```rust
let config = StoreConfig::new("/data/events")
    .with_index_layout(IndexLayout::AoSoA16)
    .with_incremental_projection(true);
```

## Projection Cache Backends

`Store::open` uses `NoCache` by default. For `project()` workloads that repeatedly fold
large event streams, use a persistent cache backend via `Store::open_with_cache`.

### NativeCache (built-in, no feature flag)

```rust
let config = StoreConfig::new("/var/lib/events");
let store = Store::open_with_native_cache(config, "/var/lib/events-cache")?;
```

## Benchmark Surfaces

Two surfaces are available:

- `cargo xtask bench --surface neutral` measures reopen/replay/write/fanout/compaction without cache backend noise.
- `cargo xtask bench --surface native` exercises the file-backed `NativeCache` projection cache hit/miss paths.

Save or compare baselines per OS and surface:

```bash
cargo xtask bench --surface neutral --save
cargo xtask bench --surface native --compare
```
