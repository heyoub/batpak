//! The Spawn seam: the reviewed public primitive for "run a unit of work somewhere
//! and give me a handle", over production threads and deterministic simulation.
//!
//! Boundary: this is the narrow room where background work becomes an OS thread.
//! Production routes through [`ThreadSpawn`], a thin wrapper over
//! [`std::thread::Builder`]. A deterministic-simulation backend (the cooperative
//! `SimScheduler`, test-scoped) implements the SAME [`Spawn`] contract, so both paths
//! satisfy one interface — the *shared-drive rule*: a job behaves identically whether
//! it runs on a real thread or the cooperative scheduler (a red fixture proves it).
//!
//! This is a **reviewed public API** (promoted from `pub(crate)` per kernel plan §12)
//! so embeddings and sibling crates can supply their own [`Spawn`] without reaching
//! into store internals. The concrete `SimScheduler`/`SimClock` stay test-scoped.
//!
//! Failures are TYPED: [`SpawnError`] on creation, [`JoinError`] on join (mirroring
//! [`std::thread::Builder::spawn`]'s `io::Error` and [`std::thread::JoinHandle::join`]'s
//! panic propagation, but as named domain errors callers match on).

use std::thread::JoinHandle as ThreadJoinHandle;

/// Typed failure to spawn a unit of work.
#[derive(Debug)]
#[non_exhaustive]
pub enum SpawnError {
    /// The backing OS thread could not be created — mirrors the `io::Error` from
    /// [`std::thread::Builder::spawn`]. The source is preserved for callers that map
    /// it to their own I/O failure.
    ThreadCreation(std::io::Error),
}

impl std::fmt::Display for SpawnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ThreadCreation(err) => {
                write!(f, "could not create the backing thread: {err}")
            }
        }
    }
}

impl std::error::Error for SpawnError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::ThreadCreation(err) => Some(err),
        }
    }
}

/// Typed failure to join a spawned job.
#[derive(Debug)]
#[non_exhaustive]
pub enum JoinError {
    /// The body panicked (unwound) — mirrors the `Err` arm of
    /// [`std::thread::Result`]. The panic payload is not surfaced (callers map this to
    /// a domain "crashed" outcome, exactly as before).
    Panicked,
}

impl std::fmt::Display for JoinError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Panicked => write!(f, "the spawned body panicked"),
        }
    }
}

impl std::error::Error for JoinError {}

/// The status of a spawned job, observed WITHOUT blocking.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobStatus {
    /// The body is still running.
    Running,
    /// The body has finished — cleanly OR by panic; the two are distinguished only by
    /// [`JobHandle::join`].
    Finished,
}

/// Handle to a spawned unit of work: a stable non-blocking status plus a typed join.
///
/// `join` consumes the boxed handle and blocks until the work finishes, so it takes
/// `self: Box<Self>` (matching the [`Spawn::spawn`] return of a boxed trait object).
pub trait JobHandle: Send + Sync {
    /// Block until the spawned body finishes.
    ///
    /// # Errors
    /// [`JoinError::Panicked`] if the body unwound (panicked), mirroring the `Err`
    /// arm of [`std::thread::JoinHandle::join`].
    fn join(self: Box<Self>) -> Result<(), JoinError>;

    /// Whether the body has already finished (clean or panicked) without blocking.
    /// Mirrors [`std::thread::JoinHandle::is_finished`]; used by liveness probes such
    /// as the writer's `fail_if_exited` crash detector.
    fn is_finished(&self) -> bool;

    /// The job's status without blocking. Defaulted from [`JobHandle::is_finished`] so
    /// every handle reports a stable [`JobStatus`] over the one contract.
    fn status(&self) -> JobStatus {
        if self.is_finished() {
            JobStatus::Finished
        } else {
            JobStatus::Running
        }
    }
}

/// Abstraction over "run this `FnOnce` somewhere and give me a handle".
///
/// Production is [`ThreadSpawn`] (one OS thread per spawn). A deterministic
/// simulation backend implements this to multiplex bodies onto a controlled scheduler
/// without changing any call site. `Send + Sync` so it can live behind
/// `Arc<dyn Spawn>` on `StoreConfig` and be shared across threads.
pub trait Spawn: Send + Sync {
    /// Spawn `body` under thread name `name`, returning a [`JobHandle`].
    ///
    /// `stack_size` mirrors [`std::thread::Builder::stack_size`]: `Some(n)` requests
    /// an explicit stack, `None` uses the OS default. Backends that do not run on OS
    /// threads may ignore it.
    ///
    /// # Errors
    /// [`SpawnError::ThreadCreation`] when the backing thread cannot be created,
    /// preserving the underlying `io::Error`.
    fn spawn(
        &self,
        name: String,
        stack_size: Option<usize>,
        body: Box<dyn FnOnce() + Send + 'static>,
    ) -> Result<Box<dyn JobHandle>, SpawnError>;
}

/// Production [`Spawn`]: one [`std::thread`] per call, behavior-identical to the
/// previous in-line `std::thread::Builder` usage.
#[derive(Debug, Default, Clone, Copy)]
pub struct ThreadSpawn;

/// [`JobHandle`] over a real [`ThreadJoinHandle`]. `join` delegates to
/// [`std::thread::JoinHandle::join`], mapping the panic payload to
/// [`JoinError::Panicked`] (callers never needed the payload, only the Err).
struct ThreadJob(ThreadJoinHandle<()>);

impl JobHandle for ThreadJob {
    fn join(self: Box<Self>) -> Result<(), JoinError> {
        self.0.join().map_err(|_| JoinError::Panicked)
    }

    fn is_finished(&self) -> bool {
        self.0.is_finished()
    }
}

impl From<SpawnError> for crate::store::error::StoreError {
    fn from(err: SpawnError) -> Self {
        match err {
            // A failed spawn is a thread-creation I/O failure; preserve the source.
            SpawnError::ThreadCreation(io) => Self::Io(io),
        }
    }
}

impl Spawn for ThreadSpawn {
    fn spawn(
        &self,
        name: String,
        stack_size: Option<usize>,
        body: Box<dyn FnOnce() + Send + 'static>,
    ) -> Result<Box<dyn JobHandle>, SpawnError> {
        let mut builder = std::thread::Builder::new().name(name);
        if let Some(stack_size) = stack_size {
            builder = builder.stack_size(stack_size);
        }
        let handle = builder.spawn(body).map_err(SpawnError::ThreadCreation)?;
        Ok(Box::new(ThreadJob(handle)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn thread_spawn_runs_body_and_join_returns_ok() {
        let spawner: Arc<dyn Spawn> = Arc::new(ThreadSpawn);
        let flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag_for_body = Arc::clone(&flag);
        let handle = spawner
            .spawn(
                "thread-spawn-ok-proof".to_string(),
                None,
                Box::new(move || {
                    flag_for_body.store(true, std::sync::atomic::Ordering::Release);
                }),
            )
            .expect("spawn must succeed");
        handle.join().expect("clean body must join Ok");
        assert!(
            flag.load(std::sync::atomic::Ordering::Acquire),
            "PROPERTY: ThreadSpawn must run the supplied body to completion"
        );
    }

    #[test]
    fn thread_spawn_join_surfaces_panic_as_err() {
        let spawner = ThreadSpawn;
        let handle = spawner
            .spawn(
                "thread-spawn-panic-proof".to_string(),
                Some(256 * 1024),
                Box::new(|| {
                    // Deterministically unwind this spawned body to prove
                    // JobHandle::join surfaces the panic as Err(JoinError::Panicked).
                    // `black_box` hides the `None` from the literal-unwrap lint;
                    // `expect` is the permitted in-test panic shape (not `panic!`).
                    std::hint::black_box(Option::<()>::None)
                        .expect("intentional spawn panic proof");
                }),
            )
            .expect("spawn must succeed");
        assert!(
            matches!(handle.join(), Err(JoinError::Panicked)),
            "PROPERTY: a panicking body surfaces through JobHandle::join as \
             JoinError::Panicked, matching std::thread::JoinHandle::join"
        );
    }

    #[test]
    fn status_is_running_until_the_body_is_released_then_joins() {
        use std::sync::atomic::Ordering;
        let spawner = ThreadSpawn;
        let gate = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let gate_for_body = Arc::clone(&gate);
        // A gated body that spins until released — cannot finish before we open it.
        let handle: Box<dyn JobHandle> = spawner
            .spawn(
                "thread-spawn-status-proof".to_string(),
                None,
                Box::new(move || {
                    while !gate_for_body.load(Ordering::Acquire) {
                        std::hint::spin_loop();
                    }
                }),
            )
            .expect("spawn must succeed");
        assert_eq!(
            handle.status(),
            JobStatus::Running,
            "PROPERTY: a gated body reports Running before release"
        );
        gate.store(true, Ordering::Release);
        handle.join().expect("released body joins Ok");
    }

    #[test]
    fn spawn_error_is_a_typed_io_failure_preserving_its_source() {
        // Name the typed spawn error; its Display wraps the io source.
        let err = SpawnError::ThreadCreation(std::io::Error::other("simulated"));
        assert!(err.to_string().contains("backing thread"));
        assert!(std::error::Error::source(&err).is_some());
    }
}
