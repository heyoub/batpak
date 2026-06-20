use super::*;
use crate::store::cold_start::rebuild::OpenIndexReport;

struct OpenComponents {
    runtime: Arc<config::ValidatedStoreConfig>,
    config: Arc<StoreConfig>,
    index: Arc<StoreIndex>,
    reader: Arc<Reader>,
    open_report: cold_start::rebuild::OpenIndexReport,
    cumulative_reserved_kind_fallbacks: segment::sidx::ReservedKindFallbackStats,
    store_lock: dir_lock::StoreDirLock,
}

fn validate_payload_registry_for_open(config: &StoreConfig) -> Result<(), StoreError> {
    let Err(error) = event::payload::cached_event_payload_registry_validation() else {
        return Ok(());
    };
    match config.event_payload_validation {
        EventPayloadValidation::Warn => {
            if event::payload::mark_event_payload_registry_warning_emitted() {
                tracing::warn!(
                    target: "batpak::event_registry",
                    collisions = ?error.collisions(),
                    "duplicate EventPayload kind registrations detected; call validate_event_payload_registry() or set EventPayloadValidation::FailFast to make this an open error"
                );
            }
            Ok(())
        }
        EventPayloadValidation::FailFast => Err(StoreError::EventPayloadRegistry(error)),
        EventPayloadValidation::Silent => Ok(()),
    }
}

fn open_components(
    mut config: StoreConfig,
    lock_mode: StoreLockMode,
) -> Result<OpenComponents, StoreError> {
    validate_payload_registry_for_open(&config)?;
    platform::fs::create_dir_all(&config.data_dir)?;
    config.data_dir = platform::fs::canonicalize(&config.data_dir).map_err(StoreError::Io)?;
    let configured_signing_keys = config.signing_keys.len();
    tracing::debug!(
        configured_signing_keys,
        "opening store with configured signing registry"
    );
    let runtime = Arc::new(config.validated()?);
    let store_lock = dir_lock::StoreDirLock::acquire(&config.data_dir, lock_mode)?;
    if let Some(profile_path) = config.platform_profile_path.as_ref() {
        let _verified_platform_evidence =
            platform::profile::PlatformProfile::verify_current_store_path(
                profile_path,
                &config.data_dir,
                runtime.clock(),
            )?;
    }
    let config = Arc::new(config);
    let index = Arc::new(StoreIndex::with_config(&config.index));
    let reader = Arc::new(Reader::new(
        config.data_dir.clone(),
        config.fd_budget,
        &runtime.clock_arc(),
    ));

    // Cold start: checkpoint/mmap fast paths or full segment scan.
    // Segment files are named so lexicographic order matches replay order.
    // The fault injector only exists when `dangerous-test-hooks` is enabled;
    // otherwise the cold-start path takes an inert `&()` (see FaultInjectorRef).
    #[cfg(feature = "dangerous-test-hooks")]
    let cold_start_fault_injector = &config.fault_injector;
    #[cfg(not(feature = "dangerous-test-hooks"))]
    let cold_start_fault_injector = &();
    let open_outcome = cold_start::rebuild::open_index(
        &index,
        &reader,
        &config.data_dir,
        runtime.cold_start,
        runtime.clock(),
        cold_start_fault_injector,
    )?;

    // Tell the reader which segment is active (for mmap dispatch).
    // The writer's initial segment ID is the highest existing + 1.
    let active_seg_id = next_active_segment_id(&config.data_dir)?;
    reader.set_active_segment(active_seg_id);

    Ok(OpenComponents {
        runtime,
        config,
        index,
        reader,
        open_report: open_outcome.report,
        cumulative_reserved_kind_fallbacks: open_outcome.cumulative_reserved_kind_fallbacks,
        store_lock,
    })
}

fn next_active_segment_id(data_dir: &std::path::Path) -> Result<u64, StoreError> {
    Ok(write::writer::find_latest_segment_id(data_dir)?.unwrap_or(0) + 1)
}

fn emit_open_report_observability(config: &StoreConfig, report: &OpenIndexReport) {
    tracing::info!(
        target: "batpak::open",
        path = ?report.path,
        restored_entries = report.restored_entries,
        tail_entries = report.tail_entries,
        elapsed_us = report.elapsed_us,
        phase_plan_build_us = report.phase_plan_build_us,
        phase_interner_us = report.phase_interner_us,
        phase_restore_index_us = report.phase_restore_index_us,
        phase_hidden_ranges_us = report.phase_hidden_ranges_us,
        unknown_reserved_system_kind_fallbacks = report.unknown_reserved_system_kind_fallbacks,
        unknown_reserved_effect_kind_fallbacks = report.unknown_reserved_effect_kind_fallbacks,
        cumulative_unknown_reserved_system_kind_fallbacks = report
            .cumulative_unknown_reserved_system_kind_fallbacks,
        cumulative_unknown_reserved_effect_kind_fallbacks = report
            .cumulative_unknown_reserved_effect_kind_fallbacks,
        unknown_reserved_system_kind_histogram = ?report.unknown_reserved_system_kind_histogram,
        unknown_reserved_effect_kind_histogram = ?report.unknown_reserved_effect_kind_histogram,
        cumulative_unknown_reserved_system_kind_histogram =
            ?report.cumulative_unknown_reserved_system_kind_histogram,
        cumulative_unknown_reserved_effect_kind_histogram =
            ?report.cumulative_unknown_reserved_effect_kind_histogram,
        "store open completed"
    );

    let Some(observer) = config.open_report_observer.as_ref() else {
        return;
    };
    let observer = Arc::clone(observer);
    if catch_unwind(AssertUnwindSafe(|| observer(report))).is_err() {
        tracing::warn!(
            target: "batpak::open",
            "open report observer panicked; continuing with successful open"
        );
    }
}

fn highest_index_hlc(index: &StoreIndex) -> HlcPoint {
    index
        .all_entries()
        .into_iter()
        .map(|entry| HlcPoint {
            wall_ms: entry.wall_ms,
            global_sequence: entry.global_sequence,
        })
        .max()
        .unwrap_or(HlcPoint::ORIGIN)
}

fn last_close_hlc(index: &StoreIndex) -> Result<HlcPoint, StoreError> {
    let mut close_points: Vec<_> = index
        .all_entries()
        .into_iter()
        .filter(|entry| entry.kind == EventKind::SYSTEM_CLOSE_COMPLETED)
        .map(|entry| HlcPoint {
            wall_ms: entry.wall_ms,
            global_sequence: entry.global_sequence,
        })
        .collect();
    close_points.sort_by_key(|point| point.global_sequence);

    let mut latest = HlcPoint::ORIGIN;
    for close_hlc in close_points {
        if close_hlc < latest {
            return Err(StoreError::InvariantViolation {
                kind: StoreInvariant::CloseHlcRegression {
                    previous: latest,
                    later: close_hlc,
                },
            });
        }
        latest = close_hlc;
    }

    Ok(latest)
}

fn lifecycle_open_candidate(
    runtime: &config::ValidatedStoreConfig,
    max_recovered_hlc: HlcPoint,
    last_close_hlc: HlcPoint,
) -> Result<HlcPoint, StoreError> {
    let now_ms = match config::wall_ms_from_timestamp_us(runtime.now_us()) {
        Ok(now_ms) => now_ms,
        Err(StoreError::InvalidClock { .. }) => 0,
        Err(error) => return Err(error),
    };
    Ok(max_recovered_hlc.max(last_close_hlc).max(HlcPoint {
        wall_ms: now_ms,
        global_sequence: max_recovered_hlc.global_sequence,
    }))
}

fn validate_bootstrap_hlc(
    open_hlc: HlcPoint,
    max_recovered_hlc: HlcPoint,
    last_close_hlc: HlcPoint,
) -> Result<(), StoreError> {
    if open_hlc < max_recovered_hlc || open_hlc < last_close_hlc {
        return Err(StoreError::InvariantViolation {
            kind: StoreInvariant::BootstrapHlcOutOfOrder {
                open_hlc,
                max_recovered_hlc,
                last_close_hlc,
            },
        });
    }
    Ok(())
}

fn bootstrap_open_hlc(
    runtime: &config::ValidatedStoreConfig,
    index: &StoreIndex,
) -> Result<HlcPoint, StoreError> {
    let max_recovered_hlc = highest_index_hlc(index);
    let last_close_hlc = last_close_hlc(index)?;
    let open_hlc = lifecycle_open_candidate(runtime, max_recovered_hlc, last_close_hlc)?;
    validate_bootstrap_hlc(open_hlc, max_recovered_hlc, last_close_hlc)?;
    Ok(open_hlc)
}

pub(super) fn timestamp_us_for_hlc(point: HlcPoint) -> Result<i64, StoreError> {
    let timestamp_us =
        point
            .wall_ms
            .checked_mul(1000)
            .ok_or_else(|| StoreError::InvariantViolation {
                kind: StoreInvariant::OpenHlcWallMsOverflow {
                    wall_ms: point.wall_ms,
                },
            })?;
    i64::try_from(timestamp_us).map_err(|_| StoreError::InvariantViolation {
        kind: StoreInvariant::OpenHlcTimestampOutOfRange {
            wall_ms: point.wall_ms,
        },
    })
}

fn append_open_completed_event(
    store: &Store<Open>,
    report: &OpenIndexReport,
    open_candidate: HlcPoint,
) -> Result<HlcPoint, StoreError> {
    let coord = Coordinate::new("batpak:store", "batpak:lifecycle")?;
    let submission = AppendSubmission::with_options(
        AppendOptions::default().with_idempotency(crate::id::IdempotencyKey::from(
            crate::id::generate_v7_id_with_clock(store.runtime.clock()),
        )),
        store.runtime.clock(),
    );
    submission.validate_route(store)?;
    submission.validate_idempotency(store)?;
    let event = submission.build_event(
        report,
        EventKind::SYSTEM_OPEN_COMPLETED,
        timestamp_us_for_hlc(open_candidate)?,
    )?;

    let (tx, rx) = flume::bounded(1);
    let command = submission.into_command(coord, EventKind::SYSTEM_OPEN_COMPLETED, event, tx);
    store
        .writer_handle()?
        .tx
        .send(command)
        .map_err(|_| StoreError::WriterCrashed)?;
    let receipt = recv_writer_reply(&rx)?;
    let receipt_event_id_raw = {
        use crate::id::EntityIdType;
        receipt.event_id.as_u128()
    };
    let open_hlc = store
        .index
        .get_by_id(receipt_event_id_raw)
        .map(|entry| HlcPoint {
            wall_ms: entry.wall_ms,
            global_sequence: entry.global_sequence,
        })
        .ok_or_else(|| StoreError::InvariantViolation {
            kind: StoreInvariant::OpenReceiptNotIndexed {
                event_id: receipt_event_id_raw,
            },
        })?;
    validate_bootstrap_hlc(open_hlc, open_candidate, last_close_hlc(&store.index)?)?;
    Ok(open_hlc)
}

impl Store<Open> {
    /// Open a store at the given config's data directory. Creates the directory if absent.
    /// Uses `NoCache` for projection (no external cache backend).
    ///
    /// # Errors
    /// Returns [`StoreError::StoreLocked`] if another live store handle already
    /// owns the directory lock.
    /// Returns `StoreError::Io` if the data directory cannot be created or segments cannot be read.
    pub fn open(config: StoreConfig) -> Result<Self, StoreError> {
        Self::open_with_cache(config, Box::new(NoCache))
    }

    /// Open a store with the built-in file-backed projection cache.
    ///
    /// # Errors
    /// Returns [`StoreError::CacheFailed`] if the cache directory cannot be created,
    /// or any error from [`Store::open_with_cache`].
    pub fn open_with_native_cache(
        config: StoreConfig,
        cache_path: impl AsRef<std::path::Path>,
    ) -> Result<Self, StoreError> {
        Self::open_with_cache(config, Box::new(NativeCache::open(cache_path)?))
    }

    /// Open a store with a custom projection cache backend.
    /// Use [`NativeCache`] for file-backed cache-accelerated `project()` calls.
    ///
    /// # Errors
    /// Returns [`StoreError::StoreLocked`] if another live store handle already
    /// owns the directory lock.
    /// Returns `StoreError::Io` if the data directory cannot be created or segments cannot be read.
    pub fn open_with_cache(
        config: StoreConfig,
        cache: Box<dyn ProjectionCache>,
    ) -> Result<Self, StoreError> {
        let OpenComponents {
            runtime,
            config,
            index,
            reader,
            open_report,
            cumulative_reserved_kind_fallbacks,
            store_lock,
        } = open_components(config, StoreLockMode::Mutable)?;

        let open_candidate = bootstrap_open_hlc(&runtime, &index)?;
        let subscribers = Arc::new(SubscriberList::new());
        let reactor_subscribers = Arc::new(ReactorSubscriberList::new());
        let writer = WriterHandle::spawn(
            &config,
            &runtime,
            &index,
            &subscribers,
            &reactor_subscribers,
            &reader,
        )?;
        let watermark_handle = writer.watermark_handle();
        let projection_registry = ProjectionRegistry::new(watermark_handle.clone());

        let store = Self {
            index,
            reader,
            cache,
            writer: Some(writer),
            watermark_handle,
            projection_registry,
            lifecycle_gate: Mutex::new(()),
            config,
            runtime,
            should_shutdown_on_drop: true,
            open_report: Some(open_report.clone()),
            cumulative_reserved_kind_fallbacks,
            _state: std::marker::PhantomData,
            _store_lock: store_lock,
        };

        emit_open_report_observability(&store.config, &open_report);
        let open_hlc = append_open_completed_event(&store, &open_report, open_candidate)?;
        lifecycle::sync(&store)?;
        store.watermark_handle.lock().reset_to_bootstrap(open_hlc);

        Ok(store)
    }
}

impl Store<ReadOnly> {
    /// Open the store without starting a writer thread.
    ///
    /// # Errors
    /// Returns any configuration, directory-creation, or cold-start rebuild
    /// error surfaced while opening the store in read-only mode.
    pub fn open_read_only(config: StoreConfig) -> Result<Self, StoreError> {
        Self::open_read_only_with_cache(config, Box::new(NoCache))
    }

    /// Open the store in read-only mode with the built-in projection cache.
    ///
    /// # Errors
    /// Returns [`StoreError::CacheFailed`] if the native cache cannot be
    /// opened, or any error returned by [`Store::open_read_only_with_cache`].
    pub fn open_read_only_with_native_cache(
        config: StoreConfig,
        cache_path: impl AsRef<std::path::Path>,
    ) -> Result<Self, StoreError> {
        Self::open_read_only_with_cache(config, Box::new(NativeCache::open(cache_path)?))
    }

    /// Open the store in read-only mode with a custom projection cache backend.
    ///
    /// # Errors
    /// Returns [`StoreError::StoreLocked`] if another live store handle already
    /// owns the directory lock. Read-only opens are also exclusive under the
    /// current store-ownership contract.
    /// Returns any configuration, directory-creation, or cold-start rebuild
    /// error surfaced while opening the store in read-only mode.
    pub fn open_read_only_with_cache(
        config: StoreConfig,
        cache: Box<dyn ProjectionCache>,
    ) -> Result<Self, StoreError> {
        let OpenComponents {
            runtime,
            config,
            index,
            reader,
            open_report,
            cumulative_reserved_kind_fallbacks,
            store_lock,
        } = open_components(config, StoreLockMode::ReadOnly)?;

        let open_hlc = bootstrap_open_hlc(&runtime, &index)?;
        let watermark_handle = WatermarkState::bootstrap_handle(open_hlc, runtime.clock_arc());
        let projection_registry = ProjectionRegistry::new(watermark_handle.clone());
        let store = Self {
            index,
            reader,
            cache,
            writer: None,
            watermark_handle,
            projection_registry,
            lifecycle_gate: Mutex::new(()),
            config,
            runtime,
            should_shutdown_on_drop: false,
            open_report: Some(open_report.clone()),
            cumulative_reserved_kind_fallbacks,
            _state: std::marker::PhantomData,
            _store_lock: store_lock,
        };

        emit_open_report_observability(&store.config, &open_report);

        Ok(store)
    }
}

#[cfg(test)]
mod tests;
