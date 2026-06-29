//! Public Spawn seam surface (kernel plan §12): the reviewed primitive that
//! embeddings and sibling crates program against — `Spawn` + a stable status/join
//! `JobHandle` + typed `SpawnError`/`JoinError`. Promoted from `pub(crate)` to a
//! reviewed `pub` API; this test pins that public shape.

use batpak::store::{JobHandle, JobStatus, JoinError, Spawn, SpawnError, ThreadSpawn};
use std::sync::Arc;

#[test]
fn thread_spawn_runs_a_job_reports_status_then_joins() {
    let spawner: Arc<dyn Spawn> = Arc::new(ThreadSpawn);
    let handle: Box<dyn JobHandle> = spawner
        .spawn("public-spawn-surface".to_string(), None, Box::new(|| {}))
        .expect("spawn succeeds");
    // status() is the stable non-blocking probe; join() is the typed terminal.
    let status: JobStatus = handle.status();
    assert!(matches!(status, JobStatus::Running | JobStatus::Finished));
    handle.join().expect("a clean body joins Ok");
}

#[test]
fn spawn_and_join_failures_are_named_domain_errors() {
    // The seam's failures are TYPED, not raw io::Error / thread::Result.
    let spawn_err = SpawnError::ThreadCreation(std::io::Error::other("simulated"));
    assert!(spawn_err.to_string().contains("thread"));
    let join_err = JoinError::Panicked;
    assert!(join_err.to_string().contains("panicked"));
}
