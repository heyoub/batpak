//! Thread-spawn seam for production and (future) deterministic simulation.
//!
//! Boundary: this is the narrow room where store background work becomes an OS
//! thread. Production routes through [`ThreadSpawn`], which is a byte-for-byte
//! wrapper over [`std::thread::Builder`]; the only observable difference from a
//! raw `Builder::new().name(..).spawn(..)` call is the indirection through a
//! trait object. The Sim scheduler (a later gauntlet item) installs an
//! alternate [`Spawn`] that runs bodies on a cooperative scheduler — it is NOT
//! built here. This file only introduces the seam so production spawn sites
//! stop calling `std::thread` directly.
//!
//! The join contract mirrors [`std::thread::JoinHandle::join`] exactly: it
//! returns [`std::thread::Result<()>`] so callers keep their existing
//! panic-propagation handling (e.g. `.map_err(|_| StoreError::WriterCrashed)`).

use std::thread::JoinHandle;

/// Result of joining a spawned unit of work.
///
/// Identical shape to [`std::thread::Result`]: `Ok(())` on clean exit, `Err`
/// carrying the panic payload when the body unwound. Callers map the error to
/// their domain failure exactly as they did for a raw [`JoinHandle`].
pub(crate) type SimJoinResult = std::thread::Result<()>;

/// Handle to a spawned unit of work.
///
/// `join` consumes the boxed handle and blocks until the work finishes, so the
/// signature takes `self: Box<Self>` (matching the [`Spawn::spawn`] return of a
/// boxed trait object). This mirrors [`JoinHandle::join`]'s by-value consume.
pub(crate) trait SimJoin: Send + Sync {
    /// Block until the spawned body finishes, returning its completion result.
    fn join(self: Box<Self>) -> SimJoinResult;

    /// Whether the spawned body has already finished (clean or panicked),
    /// without blocking. Mirrors [`JoinHandle::is_finished`]; used by liveness
    /// probes such as the writer's `fail_if_exited` crash detector.
    fn is_finished(&self) -> bool;
}

/// Abstraction over "run this `FnOnce` somewhere and give me a join handle".
///
/// Production is [`ThreadSpawn`] (one OS thread per spawn). A deterministic
/// simulation backend can implement this to multiplex bodies onto a controlled
/// scheduler without changing any call site. `Send + Sync` so it can live
/// behind `Arc<dyn Spawn>` on `StoreConfig` and be shared across threads.
pub(crate) trait Spawn: Send + Sync {
    /// Spawn `body` under thread name `name`, returning a join handle.
    ///
    /// `stack_size` mirrors [`std::thread::Builder::stack_size`]: `Some(n)`
    /// requests an explicit stack, `None` uses the OS default. Backends that do
    /// not run on OS threads may ignore it.
    ///
    /// # Errors
    /// Returns [`std::io::Error`] when the backing thread cannot be created,
    /// matching [`std::thread::Builder::spawn`]'s failure mode so callers keep
    /// using `.map_err(StoreError::Io)`.
    fn spawn(
        &self,
        name: String,
        stack_size: Option<usize>,
        body: Box<dyn FnOnce() + Send + 'static>,
    ) -> std::io::Result<Box<dyn SimJoin>>;
}

/// Production [`Spawn`]: one [`std::thread`] per call, behavior-identical to the
/// previous in-line `std::thread::Builder` usage.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct ThreadSpawn;

/// [`SimJoin`] wrapper over a real [`JoinHandle`]. `join` delegates straight to
/// [`JoinHandle::join`], preserving panic propagation exactly.
struct ThreadJoin(JoinHandle<()>);

impl SimJoin for ThreadJoin {
    fn join(self: Box<Self>) -> SimJoinResult {
        self.0.join()
    }

    fn is_finished(&self) -> bool {
        self.0.is_finished()
    }
}

impl Spawn for ThreadSpawn {
    fn spawn(
        &self,
        name: String,
        stack_size: Option<usize>,
        body: Box<dyn FnOnce() + Send + 'static>,
    ) -> std::io::Result<Box<dyn SimJoin>> {
        let mut builder = std::thread::Builder::new().name(name);
        if let Some(stack_size) = stack_size {
            builder = builder.stack_size(stack_size);
        }
        let handle = builder.spawn(body)?;
        Ok(Box::new(ThreadJoin(handle)))
    }
}

#[cfg(test)]
mod tests {
    // justifies: INV-TEST-PANIC-AS-ASSERTION; spawn proof bodies deliberately panic to prove SimJoin::join surfaces unwinds as Err, mirroring std::thread::JoinHandle::join.
    #![allow(clippy::panic)]
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
                Box::new(|| panic!("intentional spawn panic proof")),
            )
            .expect("spawn must succeed");
        assert!(
            handle.join().is_err(),
            "PROPERTY: a panicking body must surface through SimJoin::join as Err, \
             matching std::thread::JoinHandle::join"
        );
    }
}
