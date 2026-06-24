use crate::coordinate::Coordinate;
use crate::event::EventKind;
use crate::store::lifecycle_close::write_cold_start_artifacts_on_close;
use crate::store::write::control::AppendSubmission;
use crate::store::{
    AppendOptions, Closed, Open, Store, StoreDiagnostics, StoreError, StoreStats, WriterPressure,
};
use serde::Serialize;

#[path = "lifecycle_compact.rs"]
mod lifecycle_compact;
#[path = "lifecycle_fork.rs"]
mod lifecycle_fork;
#[path = "lifecycle_fs.rs"]
mod lifecycle_fs;
#[path = "lifecycle_snapshot.rs"]
mod lifecycle_snapshot;

pub(crate) use lifecycle_compact::compact;
pub(crate) use lifecycle_fork::fork;
pub(crate) use lifecycle_snapshot::snapshot;

#[derive(Serialize)]
struct CloseLifecyclePayload {
    wall_ms: u64,
    global_sequence: u64,
}

fn append_close_completed_event(store: &Store<Open>) -> Result<(), StoreError> {
    let close_hlc = store.watermark_handle.lock().snapshot().visible_hlc;
    let coord = Coordinate::new("batpak:store", "batpak:lifecycle")?;
    let submission = AppendSubmission::with_options(
        AppendOptions::default().with_idempotency(crate::id::IdempotencyKey::from(
            crate::id::generate_v7_id_with_clock(store.runtime.clock()),
        )),
        store.runtime.clock(),
    );
    submission.validate_route(store)?;
    submission.validate_idempotency(store)?;

    let payload = CloseLifecyclePayload {
        wall_ms: close_hlc.wall_ms,
        global_sequence: close_hlc.global_sequence,
    };
    let event = submission.build_event(
        &payload,
        EventKind::SYSTEM_CLOSE_COMPLETED,
        super::timestamp_us_for_hlc(close_hlc)?,
    )?;

    let (tx, rx) = flume::bounded(1);
    let command = submission.into_command(coord, EventKind::SYSTEM_CLOSE_COMPLETED, event, tx);
    store
        .writer_handle()?
        .tx
        .send(command)
        .map_err(|_| StoreError::WriterCrashed)?;
    store.writer_handle()?.pump();
    let _ = crate::store::recv_writer_reply(&rx)?;
    Ok(())
}

pub(crate) fn sync(store: &Store<Open>) -> Result<(), StoreError> {
    tracing::debug!(target: "batpak::flow", flow = "sync");
    let (tx, rx) = flume::bounded(1);
    store
        .writer_handle()?
        .tx
        .send(crate::store::write::writer::WriterCommand::Sync { respond: tx })
        .map_err(|_| StoreError::WriterCrashed)?;
    store.writer_handle()?.pump();
    crate::store::recv_writer_reply(&rx)
}

pub(crate) fn close(mut store: Store<Open>) -> Result<Closed, StoreError> {
    tracing::debug!(target: "batpak::flow", flow = "close");
    let _lifecycle = store.lifecycle_gate.lock();
    if let Err(error) = append_close_completed_event(&store) {
        tracing::warn!(
            target: "batpak::flow",
            flow = "close",
            "failed to append SYSTEM_CLOSE_COMPLETED lifecycle event: {error}"
        );
    }

    let (tx, rx) = flume::bounded(1);
    store
        .writer_handle()?
        .tx
        .send(crate::store::write::writer::WriterCommand::Shutdown { respond: tx })
        .map_err(|_| StoreError::WriterCrashed)?;
    store.writer_handle()?.pump();
    let result = crate::store::recv_writer_reply(&rx);

    result?;
    store.state.0.join()?;

    store.index.idemp.flush(&store.config.data_dir)?;

    write_cold_start_artifacts_on_close(&store)?;

    store.should_shutdown_on_drop = false;
    Ok(Closed)
}

pub(crate) fn stats<State: crate::store::StoreState>(store: &Store<State>) -> StoreStats {
    StoreStats {
        event_count: store.index.len(),
        global_sequence: store.index.global_sequence(),
    }
}

pub(crate) fn diagnostics<State: crate::store::StoreState>(
    store: &Store<State>,
) -> StoreDiagnostics {
    let frontier = store.watermark_handle.lock().snapshot_view();
    StoreDiagnostics {
        event_count: store.index.len(),
        global_sequence: store.index.global_sequence(),
        visible_sequence: store.index.visible_sequence(),
        data_dir: store.config.data_dir.clone(),
        segment_max_bytes: store.config.segment_max_bytes,
        fd_budget: store.config.fd_budget,
        restart_policy: store.config.writer.restart_policy.clone(),
        writer_pressure: store
            .state
            .writer_queue_len()
            .map(|queue_len| WriterPressure {
                queue_len,
                capacity: store.config.writer.channel_capacity,
            })
            .unwrap_or(WriterPressure {
                queue_len: 0,
                capacity: 0,
            }),
        frontier,
        index_topology: store.index.topology_name(),
        tile_count: store.index.tile_count(),
        open_report: store.open_report.clone(),
        platform_evidence: crate::store::platform::evidence::collect_for_store_path(
            &store.config.data_dir,
            store.runtime.clock(),
        ),
    }
}
