# Cache backends

Use:

- `Store::open_with_native_cache(config, cache_path)` for the built-in file-backed projection cache (no feature flag required)

This wraps the lower-level `open_with_cache` API for common setups.
