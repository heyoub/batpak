use super::*;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

#[test]
fn validated_runtime_clock_wraps_direct_field_assignment() {
    let raw = Arc::new(AtomicI64::new(2_000));
    let raw_clock = {
        let raw = Arc::clone(&raw);
        Arc::new(move || raw.load(Ordering::SeqCst)) as Arc<dyn Fn() -> i64 + Send + Sync>
    };

    let mut config = StoreConfig::new("target/test-clock-wrap");
    config.clock = Some(clock_from_fn(raw_clock));

    let runtime = config.validated().expect("config validates");
    assert_eq!(runtime.now_us(), 2_000);

    raw.store(1_500, Ordering::SeqCst);
    assert_eq!(
        runtime.now_us(),
        2_000,
        "validated runtime clock must clamp direct-field regressions"
    );
}

#[test]
fn cache_now_us_clamps_negative_custom_clock_values() {
    let raw_clock = Arc::new(|| -42_i64) as Arc<dyn Fn() -> i64 + Send + Sync>;
    let mut config = StoreConfig::new("target/test-cache-clock-clamp");
    config.clock = Some(clock_from_fn(raw_clock));

    let runtime = config.validated().expect("config validates");
    assert_eq!(
        runtime.cache_now_us(),
        0,
        "projection/cache metadata clock must not persist negative timestamps"
    );
}

#[test]
fn cache_now_us_preserves_zero_custom_clock_value() {
    let raw_clock = Arc::new(|| 0_i64) as Arc<dyn Fn() -> i64 + Send + Sync>;
    let mut config = StoreConfig::new("target/test-cache-clock-zero");
    config.clock = Some(clock_from_fn(raw_clock));

    let runtime = config.validated().expect("config validates");
    assert_eq!(
        runtime.cache_now_us(),
        0,
        "PROPERTY: zero is a valid cache timestamp boundary, not a negative-clock violation"
    );
}

#[test]
fn validated_accepts_documented_inclusive_upper_bounds() {
    let mut config = StoreConfig::new("target/test-config-upper-bounds");
    config.writer.pressure_retry_threshold_pct = 100;
    config.batch.max_size = 4096;

    config
        .validated()
        .expect("documented inclusive upper bounds should validate");
}

#[test]
fn validated_rejects_values_above_documented_upper_bounds() {
    let mut pressure = StoreConfig::new("target/test-config-pressure-too-high");
    pressure.writer.pressure_retry_threshold_pct = 101;
    assert!(
        matches!(
            pressure.validated(),
            Err(crate::store::StoreError::Configuration(_))
        ),
        "PROPERTY: pressure retry threshold above 100 must be rejected"
    );

    let mut batch = StoreConfig::new("target/test-config-batch-too-large");
    batch.batch.max_size = 4097;
    assert!(
        matches!(
            batch.validated(),
            Err(crate::store::StoreError::Configuration(_))
        ),
        "PROPERTY: batch.max_size above 4096 must be rejected"
    );

    let mut single_append = StoreConfig::new("target/test-config-single-append-too-large");
    single_append.single_append_max_bytes = 64 * 1024 * 1024 + 1;
    assert!(
        matches!(
            single_append.validated(),
            Err(crate::store::StoreError::Configuration(_))
        ),
        "PROPERTY: single_append_max_bytes above 64MB must be rejected"
    );

    let mut batch_bytes = StoreConfig::new("target/test-config-batch-bytes-too-large");
    batch_bytes.batch.max_bytes = 16 * 1024 * 1024 + 1;
    assert!(
        matches!(
            batch_bytes.validated(),
            Err(crate::store::StoreError::Configuration(_))
        ),
        "PROPERTY: batch.max_bytes above 16MB must be rejected"
    );
}

#[test]
fn validated_rejects_zero_payload_size_boundaries() {
    let mut single_append = StoreConfig::new("target/test-config-single-append-zero");
    single_append.single_append_max_bytes = 0;
    assert!(
        matches!(
            single_append.validated(),
            Err(crate::store::StoreError::Configuration(_))
        ),
        "PROPERTY: single_append_max_bytes of zero must be rejected"
    );

    let mut batch_bytes = StoreConfig::new("target/test-config-batch-bytes-zero");
    batch_bytes.batch.max_bytes = 0;
    assert!(
        matches!(
            batch_bytes.validated(),
            Err(crate::store::StoreError::Configuration(_))
        ),
        "PROPERTY: batch.max_bytes of zero must be rejected"
    );
}

#[test]
fn validated_config_debug_names_runtime_policy_fields() {
    let runtime = StoreConfig::new("target/test-validated-debug")
        .validated()
        .expect("config validates");
    let rendered = format!("{runtime:?}");

    assert!(
        rendered.contains("ValidatedStoreConfig")
            && rendered.contains("pressure_retry_threshold")
            && rendered.contains("group_commit_drain_budget")
            && rendered.contains("signing_registry"),
        "PROPERTY: ValidatedStoreConfig Debug must name the runtime policy fields, got: {rendered}"
    );
}

#[test]
fn process_boot_ns_is_nonzero_and_stable_in_process() {
    let clock = SystemClock::new();
    let first = clock.process_boot_ns();
    let second = clock.process_boot_ns();

    assert_ne!(
        first, 0,
        "PROPERTY: process_boot_ns must expose the captured wall-clock anchor, not zero/default"
    );
    assert_eq!(
        first, second,
        "PROPERTY: process_boot_ns must stay stable for the process lifetime"
    );
}

#[test]
fn now_mono_ns_advances_beyond_nonzero_sentinel() {
    let clock = SystemClock::new();
    std::thread::sleep(Duration::from_millis(1));
    let elapsed = clock.now_mono_ns();

    assert!(
        elapsed > 1,
        "PROPERTY: now_mono_ns must report elapsed nanoseconds from the process anchor, not a fixed sentinel; got {elapsed}"
    );
}

#[test]
fn duration_micros_preserves_zero_and_one_microsecond_boundaries() {
    assert_eq!(
        duration_micros(Duration::ZERO),
        0,
        "PROPERTY: zero duration must remain zero, not a default/nonzero sentinel"
    );
    assert_eq!(
        duration_micros(Duration::from_micros(1)),
        1,
        "PROPERTY: one microsecond must round-trip exactly"
    );
}

#[test]
fn has_custom_clock_reflects_clock_presence() {
    // Pins `has_custom_clock`: hardcoding it to `true` would claim a fresh
    // config carries an injected clock, breaking callers that branch on it.
    let mut config = StoreConfig::new("target/test-has-custom-clock");
    assert!(
        !config.has_custom_clock(),
        "a fresh config must report no custom clock"
    );

    let raw = Arc::new(|| 1_000i64) as Arc<dyn Fn() -> i64 + Send + Sync>;
    config.clock = Some(clock_from_fn(raw));
    assert!(
        config.has_custom_clock(),
        "a config with an injected clock must report a custom clock"
    );
}

#[test]
fn with_spawner_installs_custom_spawner_and_runs_body() {
    use crate::store::platform::spawn::{JobHandle, Spawn};
    use std::sync::atomic::AtomicBool;

    // A recording spawner proving that `with_spawner` rewires the seam: it
    // sets a flag when asked to spawn, then delegates the body to a real
    // ThreadSpawn so the join contract still holds end-to-end.
    struct RecordingSpawn {
        spawned: Arc<AtomicBool>,
        inner: crate::store::platform::spawn::ThreadSpawn,
    }
    impl Spawn for RecordingSpawn {
        fn spawn(
            &self,
            name: String,
            stack_size: Option<usize>,
            body: Box<dyn FnOnce() + Send + 'static>,
        ) -> Result<Box<dyn JobHandle>, crate::store::platform::spawn::SpawnError> {
            self.spawned.store(true, Ordering::Release);
            self.inner.spawn(name, stack_size, body)
        }
    }

    let spawned = Arc::new(AtomicBool::new(false));
    let spawner: Arc<dyn Spawn> = Arc::new(RecordingSpawn {
        spawned: Arc::clone(&spawned),
        inner: crate::store::platform::spawn::ThreadSpawn,
    });

    let config = StoreConfig::new("target/test-with-spawner").with_spawner(spawner);

    let ran = Arc::new(AtomicBool::new(false));
    let ran_for_body = Arc::clone(&ran);
    let handle = config
        .spawner()
        .spawn(
            "with-spawner-config-proof".to_string(),
            None,
            Box::new(move || ran_for_body.store(true, Ordering::Release)),
        )
        .expect("custom spawner must spawn");
    handle.join().expect("body must join Ok");

    assert!(
        spawned.load(Ordering::Acquire),
        "PROPERTY: with_spawner must route config.spawner() through the installed Spawn"
    );
    assert!(
        ran.load(Ordering::Acquire),
        "PROPERTY: the body handed to the custom spawner must run to completion"
    );
}

#[test]
fn with_fs_installs_custom_filesystem_backend() {
    use crate::store::platform::fs::{RealFs, StoreFs};
    use std::path::Path;
    use std::sync::atomic::AtomicBool;

    // A recording StoreFs proving that `with_fs` rewires the seam: it flags
    // when asked to create_dir_all, then delegates to RealFs so the production
    // op still happens (behavior-preserving delegation through the trait).
    struct RecordingFs {
        created: Arc<AtomicBool>,
        inner: RealFs,
    }
    impl StoreFs for RecordingFs {
        fn read_dir(&self, path: &Path) -> std::io::Result<std::fs::ReadDir> {
            self.inner.read_dir(path)
        }
        fn create_dir_all(&self, path: &Path) -> std::io::Result<()> {
            self.created.store(true, Ordering::Release);
            self.inner.create_dir_all(path)
        }
        fn create_new_file(&self, path: &Path) -> Result<std::fs::File, crate::store::StoreError> {
            self.inner.create_new_file(path)
        }
        fn sync_file_with_mode(
            &self,
            file: &std::fs::File,
            path: &Path,
            mode: &crate::store::SyncMode,
        ) -> Result<(), crate::store::StoreError> {
            self.inner.sync_file_with_mode(file, path, mode)
        }
        fn sync_file_all(&self, file: &std::fs::File, path: &Path) -> std::io::Result<()> {
            self.inner.sync_file_all(file, path)
        }
        fn sync_parent_dir(&self, path: &Path) -> Result<(), crate::store::StoreError> {
            self.inner.sync_parent_dir(path)
        }
    }

    let created = Arc::new(AtomicBool::new(false));
    let fs: Arc<dyn StoreFs> = Arc::new(RecordingFs {
        created: Arc::clone(&created),
        inner: RealFs,
    });

    let config = StoreConfig::new("target/test-with-fs").with_fs(fs);

    let dir = tempfile::tempdir().expect("tempdir");
    let nested = dir.path().join("seam").join("leaf");
    config
        .fs()
        .create_dir_all(&nested)
        .expect("custom fs must create the tree");

    assert!(
        created.load(Ordering::Acquire),
        "PROPERTY: with_fs must route config.fs() through the installed StoreFs"
    );
    assert!(
        nested.is_dir(),
        "PROPERTY: the installed StoreFs must still perform the real create_dir_all"
    );
}
