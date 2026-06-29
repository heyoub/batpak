//! The generic supervisor: it runs long-running module work off the checkout
//! path over the reviewed [`batpak::store::Spawn`] seam.
//!
//! This is deliberately *mechanism, not policy*. It spawns named bodies, reports
//! their non-blocking [`JobStatus`], and joins them on shutdown. It holds no job
//! semantics of its own — a module that needs `drive_step`-style cooperative work
//! supplies that inside its body and signals wind-down from a shutdown hook. The
//! same `Spawn` contract means a host driven by the production [`ThreadSpawn`] and
//! one driven by the cooperative test scheduler behave identically (the
//! shared-drive rule, proven at the seam in core).
//!
//! [`JobStatus`]: batpak::store::JobStatus
//! [`ThreadSpawn`]: batpak::store::ThreadSpawn

use std::sync::Arc;

use batpak::store::{JobHandle, JobStatus, JoinError, Spawn, SpawnError};

/// One spawned unit of supervised work.
struct SupervisedJob {
    name: String,
    handle: Box<dyn JobHandle>,
}

/// Owns the host's background jobs over a single [`Spawn`] backend.
pub struct Supervisor {
    spawn: Arc<dyn Spawn>,
    running: Vec<SupervisedJob>,
}

impl Supervisor {
    /// Create a supervisor backed by `spawn` (production [`batpak::store::ThreadSpawn`]
    /// or a test scheduler).
    #[must_use]
    pub fn new(spawn: Arc<dyn Spawn>) -> Self {
        Self {
            spawn,
            running: Vec::new(),
        }
    }

    /// Spawn `body` as a named supervised job.
    ///
    /// # Errors
    /// [`SpawnError`] if the backing [`Spawn`] could not start the body.
    pub fn spawn(
        &mut self,
        name: impl Into<String>,
        stack_size: Option<usize>,
        body: Box<dyn FnOnce() + Send + 'static>,
    ) -> Result<(), SpawnError> {
        let name = name.into();
        let handle = self.spawn.spawn(name.clone(), stack_size, body)?;
        self.running.push(SupervisedJob { name, handle });
        Ok(())
    }

    /// Number of jobs the supervisor is tracking (finished or running).
    #[must_use]
    pub fn job_count(&self) -> usize {
        self.running.len()
    }

    /// Whether every tracked job has finished (clean or panicked) without
    /// blocking.
    #[must_use]
    pub fn all_finished(&self) -> bool {
        self.running.iter().all(|job| job.handle.is_finished())
    }

    /// The non-blocking status of every tracked job, in spawn order.
    #[must_use]
    pub fn statuses(&self) -> Vec<(String, JobStatus)> {
        self.running
            .iter()
            .map(|job| (job.name.clone(), job.handle.status()))
            .collect()
    }

    /// Join every tracked job, blocking until each finishes, and return each
    /// job's name with its join outcome in spawn order. The supervisor is left
    /// empty.
    ///
    /// A job whose body never returns blocks here forever — the generic
    /// supervisor cannot cancel a body it did not author. Modules that own
    /// long-lived work signal it to wind down from a shutdown hook (which the
    /// host runs before this join).
    #[must_use]
    pub fn join_all(&mut self) -> Vec<(String, Result<(), JoinError>)> {
        let jobs = std::mem::take(&mut self.running);
        jobs.into_iter()
            .map(|job| (job.name, job.handle.join()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::Supervisor;
    use batpak::store::{JobStatus, ThreadSpawn};
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::sync::Arc;

    #[test]
    fn spawns_runs_and_joins_a_body() {
        let mut supervisor = Supervisor::new(Arc::new(ThreadSpawn));
        let ran = Arc::new(AtomicBool::new(false));
        let ran_in_body = Arc::clone(&ran);
        supervisor
            .spawn(
                "unit",
                None,
                Box::new(move || ran_in_body.store(true, Ordering::Release)),
            )
            .expect("ThreadSpawn starts the body");
        assert_eq!(supervisor.job_count(), 1);
        let outcomes = supervisor.join_all();
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].0, "unit");
        assert!(outcomes[0].1.is_ok(), "a clean body joins Ok");
        assert!(
            ran.load(Ordering::Acquire),
            "PROPERTY: the supervisor runs the supplied body to completion",
        );
        assert_eq!(supervisor.job_count(), 0, "join_all empties the supervisor");
    }

    #[test]
    fn join_surfaces_a_panicking_body_without_losing_siblings() {
        let mut supervisor = Supervisor::new(Arc::new(ThreadSpawn));
        let cleaned = Arc::new(AtomicU32::new(0));
        // A panicking job between two clean ones: join_all must report all three.
        for index in 0..3 {
            let cleaned_in_body = Arc::clone(&cleaned);
            supervisor
                .spawn(
                    format!("job-{index}"),
                    None,
                    Box::new(move || {
                        if index == 1 {
                            std::hint::black_box(Option::<()>::None)
                                .expect("intentional supervised-job panic proof");
                        }
                        cleaned_in_body.fetch_add(1, Ordering::AcqRel);
                    }),
                )
                .expect("spawn");
        }
        let outcomes = supervisor.join_all();
        assert_eq!(outcomes.len(), 3, "every spawned job is joined");
        assert!(outcomes[0].1.is_ok());
        assert!(
            outcomes[1].1.is_err(),
            "PROPERTY: a panicking body surfaces as a join error",
        );
        assert!(outcomes[2].1.is_ok());
        assert_eq!(
            cleaned.load(Ordering::Acquire),
            2,
            "the two clean siblings still ran",
        );
    }

    #[test]
    fn status_tracks_a_gated_body_until_released() {
        let mut supervisor = Supervisor::new(Arc::new(ThreadSpawn));
        let gate = Arc::new(AtomicBool::new(false));
        let gate_in_body = Arc::clone(&gate);
        supervisor
            .spawn(
                "gated",
                None,
                Box::new(move || {
                    while !gate_in_body.load(Ordering::Acquire) {
                        std::hint::spin_loop();
                    }
                }),
            )
            .expect("spawn");
        assert_eq!(
            supervisor.statuses(),
            vec![("gated".to_owned(), JobStatus::Running)],
            "a gated body reports Running before release",
        );
        gate.store(true, Ordering::Release);
        let outcomes = supervisor.join_all();
        assert!(outcomes[0].1.is_ok(), "released body joins Ok");
    }
}
