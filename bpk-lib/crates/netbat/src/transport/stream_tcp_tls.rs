//! Single-threaded TLS subscription session (`feature = "tls"`).
//!
//! The plaintext subscription path `try_clone`s the socket and runs a
//! control-frame READER thread alongside the delivery WRITER. That split is
//! impossible over TLS: a rustls `Connection` is stateful record-layer
//! machinery whose reads and writes both mutate it, so touching it from two
//! threads is unsound, and `StreamOwned` is not cloneable. This module therefore
//! serves the WHOLE session on ONE worker thread, multiplexing client control
//! reads with delivery writes over the single stream:
//!
//!   * Deliveries are driven by [`SubscriptionSession::poll`], whose wait is the
//!     runtime's event/watermark wakeup (a `recv_timeout` on the store
//!     subscription, NOT a sleep) — identical to the plaintext writer's cadence,
//!     so there is no busy-wait.
//!   * Between polls the loop drains any pending client control frames with a
//!     NON-BLOCKING rustls read (the socket is flipped non-blocking only for the
//!     drain), so a control read returns immediately when no frame is on the
//!     wire and never starves deliveries. Decoded frames are forwarded over the
//!     same bounded flume control lane the session already consumes, so the
//!     session's ACK/cancel handling is byte-for-byte the plaintext behavior.
//!   * Delivery WRITES happen with the socket BLOCKING, so `write_all`'s
//!     back-pressure (and the configured write timeout) behave exactly as on the
//!     plaintext writer — no busy-wait on a slow consumer.

use std::io::{self, Read};
use std::net::TcpStream;

use flume::TrySendError;
use syncbat::{SessionControl, SessionPoll, SubscriptionSession, SubscriptionSessionFactory};

use super::super::error::NetbatError;
use super::super::limits::Limits;
use super::super::stream_frame::SubscriptionToken;
use super::super::tcp::read_line;
use super::super::tls::{TlsServerConfig, TlsStream};
use super::{
    classify_control_line, decode_subscribe_request, map_runtime_error, open_session_for_subscribe,
    terminal_delivery, write_delivery, TcpSubscriptionServeStats, TcpSubscriptionServerConfig,
    SUBSCRIPTION_POLL_INTERVAL,
};

/// Plaintext-buffer chunk size for one non-blocking rustls plaintext read.
const DRAIN_READ_CHUNK: usize = 4096;

/// Upper bound on `read_tls` pulls in a single control drain. The flume control
/// lane is bounded (so a flood of well-formed ACKs self-limits via
/// [`ControlDrain::Backpressure`]) and an unterminated line self-limits via the
/// line cap, but a peer that floods empty/non-data TLS records carries no
/// plaintext to trip either bound; this cap guarantees the drain still yields
/// back to the delivery poll under such a flood. Picked up again next iteration.
const MAX_TLS_READS_PER_DRAIN: usize = 64;

/// Serve one accepted subscription session over server-only rustls TLS.
///
/// Runs the rustls handshake on the caller's (worker) thread, post-permit. A
/// handshake failure (cleartext peer, garbage ClientHello, handshake read
/// timeout, etc.) is counted in
/// [`TcpSubscriptionServeStats::tls_handshake_failures`] and the session is
/// dropped with `Ok(stats)` — never listener-fatal, mirroring the request
/// listener's `serve_tls_connection`. On a completed handshake the session is
/// served by [`run_tls_subscription_loop`].
///
/// # Errors
/// A peer-driven IO failure on a delivery write surfaces as [`NetbatError::Io`]
/// (the worker counts it as a connection IO failure); a runtime poll failure or
/// an invalid runtime config surfaces as the mapped [`NetbatError`].
pub(super) fn serve_tls_subscription_connection(
    stream: TcpStream,
    tls: &TlsServerConfig,
    runtime: &(impl SubscriptionSessionFactory + ?Sized),
    config: &TcpSubscriptionServerConfig,
) -> Result<TcpSubscriptionServeStats, NetbatError> {
    let mut stats = TcpSubscriptionServeStats::default();
    let limits = &config.limits;
    let mut tls_stream = match tls.handshake(stream) {
        Ok(tls_stream) => tls_stream,
        Err(_error) => {
            stats.tls_handshake_failures += 1;
            tracing::debug!("tls subscription handshake failed; dropping session");
            return Ok(stats);
        }
    };

    // First frame must be SUBSCRIBE, read with the socket BLOCKING (it starts
    // blocking from the accept loop). Mirrors the plaintext entry's first read.
    let first_line = match read_line(&mut tls_stream, limits.max_line_bytes) {
        Ok(line) => line,
        Err(NetbatError::EmptyStream) => return Ok(stats),
        Err(error) => return Err(error),
    };
    let subscribe = match decode_subscribe_request(&first_line, limits) {
        Ok(frame) => frame,
        Err(_) => {
            stats.failed_subscriptions += 1;
            stats.malformed_pre_subscribe += 1;
            return Ok(stats);
        }
    };

    // The session consumes control input through this lane exactly as on the
    // plaintext path; here the single session thread is the lane's only
    // producer (the drain) instead of a separate reader thread.
    let (control_tx, control_rx) = flume::bounded(16);
    let mut session = match open_session_for_subscribe(
        runtime,
        &subscribe,
        &mut tls_stream,
        limits,
        control_rx,
        &mut stats,
    )? {
        Some(session) => session,
        None => return Ok(stats),
    };
    stats.served_subscriptions += 1;
    run_tls_subscription_loop(
        &mut tls_stream,
        session.as_mut(),
        &control_tx,
        limits,
        &subscribe.subscription_id,
    )?;
    Ok(stats)
}

/// The single-threaded delivery+control multiplex.
///
/// Each iteration: (1) opportunistically drain pending client control frames
/// (non-blocking, unless the control stream is already finished), then (2) poll
/// the session for one delivery, blocking up to [`SUBSCRIPTION_POLL_INTERVAL`]
/// on the runtime's event/watermark wakeup and writing any delivery BLOCKING.
///
/// Concurrency-correctness notes a reviewer should scrutinize:
///   * The ONLY blocking wait is `session.poll`; it parks on the store
///     subscription's `recv_timeout`, so an idle session sleeps on a real signal
///     (no spin) and a busy one returns immediately with work. The control drain
///     is always non-blocking, so it cannot delay a ready delivery.
///   * Control frames the drain decodes are forwarded with `try_send`; the
///     session drains the lane fully on every `poll`, so the bounded lane is
///     empty at the start of each drain and a `Full` is transient back-pressure
///     (the unsent line stays buffered and is retried after the next poll),
///     never a single-thread deadlock or a dropped frame.
///   * A peer close / read error during the drain is modelled as a graceful
///     `Disconnected` (exactly as the plaintext reader treats a read-side IO
///     error), forwarded once it fits in the lane; the session then ends on its
///     next poll. Only a delivery-WRITE IO error propagates as `NetbatError::Io`
///     for the worker to count as a connection IO failure — matching the
///     plaintext writer's split of read-side (graceful) vs write-side (counted).
fn run_tls_subscription_loop(
    tls: &mut TlsStream,
    session: &mut dyn SubscriptionSession,
    control_tx: &flume::Sender<SessionControl>,
    limits: &Limits,
    subscription_id: &SubscriptionToken,
) -> Result<(), NetbatError> {
    let mut accumulator = ControlAccumulator::new();
    // Set once the client control stream is finished (peer gone, or a terminal
    // control frame already forwarded): stop reading the socket, just keep
    // polling until the session reports the end.
    let mut control_finished = false;
    // A peer disconnect we still owe the session but could not enqueue yet
    // (lane full); retried each iteration until it lands so the session is
    // guaranteed to learn the peer left.
    let mut disconnect_pending = false;

    loop {
        if disconnect_pending {
            if control_tx.try_send(SessionControl::Disconnected).is_ok() {
                disconnect_pending = false;
            }
        } else if !control_finished {
            tls.sock.set_nonblocking(true).map_err(NetbatError::from)?;
            let drain =
                drain_control_frames(tls, &mut accumulator, control_tx, limits, subscription_id);
            // Restore BLOCKING for the delivery write (back-pressure). Run
            // unconditionally: `drain_control_frames` never early-returns the
            // function, so this always re-blocks the socket.
            tls.sock.set_nonblocking(false).map_err(NetbatError::from)?;
            match drain {
                ControlDrain::Idle | ControlDrain::Backpressure => {}
                ControlDrain::Stopped => control_finished = true,
                ControlDrain::PeerGone => {
                    control_finished = true;
                    disconnect_pending = true;
                }
            }
        }

        match session.poll(SUBSCRIPTION_POLL_INTERVAL) {
            Ok(SessionPoll::Delivery(delivery)) => {
                write_delivery(tls, &delivery, limits)?;
                if terminal_delivery(&delivery) {
                    return Ok(());
                }
            }
            Ok(SessionPoll::Blocked) => {}
            Ok(SessionPoll::Ended) => return Ok(()),
            Err(error) => return Err(map_runtime_error(&error)),
        }
    }
}

/// Outcome of one non-blocking control drain pass.
enum ControlDrain {
    /// Nothing more readable right now; keep serving.
    Idle,
    /// The control lane filled mid-drain; the unsent line stays buffered and is
    /// retried after the next poll drains the lane. Keep serving.
    Backpressure,
    /// A terminal control frame (cancel / malformed) was forwarded; stop reading
    /// the socket. The session ends on its next poll.
    PeerGone,
    /// The peer closed or its read failed; the caller forwards `Disconnected`.
    Stopped,
}

/// Drain every client control frame currently available WITHOUT blocking.
///
/// The socket is non-blocking for the duration of this call (set by the caller).
/// Buffered plaintext is pulled from rustls first and only when none remains is
/// the socket read for more TLS records, so already-decrypted frames are never
/// stranded behind a `WouldBlock`. Partial lines persist in `accumulator` across
/// calls, so a frame split across reads is reassembled correctly.
fn drain_control_frames(
    tls: &mut TlsStream,
    accumulator: &mut ControlAccumulator,
    control_tx: &flume::Sender<SessionControl>,
    limits: &Limits,
    subscription_id: &SubscriptionToken,
) -> ControlDrain {
    let mut tls_reads = 0_usize;
    loop {
        match accumulator.forward_complete_lines(control_tx, limits, subscription_id) {
            LineFlow::NeedMore => {}
            LineFlow::Backpressure => return ControlDrain::Backpressure,
            LineFlow::Stopped => return ControlDrain::Stopped,
        }

        // Pull already-decrypted plaintext first; rustls buffers it independent
        // of the socket, so this never blocks even mid-record.
        let mut scratch = [0_u8; DRAIN_READ_CHUNK];
        match tls.conn.reader().read(&mut scratch) {
            Ok(0) => return ControlDrain::PeerGone, // clean close_notify
            Ok(count) => {
                accumulator.extend(&scratch[..count]);
                continue;
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            // EOF without close_notify, once buffered plaintext is drained.
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => {
                return ControlDrain::PeerGone
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return ControlDrain::PeerGone,
        }

        // No buffered plaintext: pull more TLS records off the socket.
        tls_reads += 1;
        if tls_reads > MAX_TLS_READS_PER_DRAIN {
            return ControlDrain::Idle;
        }
        match tls.conn.read_tls(&mut tls.sock) {
            Ok(0) => return ControlDrain::PeerGone, // socket EOF
            Ok(_) => {
                if tls.conn.process_new_packets().is_err() {
                    return ControlDrain::PeerGone;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return ControlDrain::Idle,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return ControlDrain::PeerGone,
        }
    }
}

/// Result of trying to forward every complete control line currently buffered.
enum LineFlow {
    /// No complete line remains and the partial is within the line cap.
    NeedMore,
    /// The control lane filled; the offending line stays buffered for retry.
    Backpressure,
    /// A terminal control frame was forwarded; stop draining.
    Stopped,
}

/// Reassembles control frames from the decrypted TLS plaintext byte stream.
///
/// Holds the partial trailing line across drains. A line is removed from the
/// buffer only AFTER its decoded control is accepted by the lane, so a `Full`
/// lane never drops a frame.
struct ControlAccumulator {
    buffer: Vec<u8>,
}

impl ControlAccumulator {
    fn new() -> Self {
        Self { buffer: Vec::new() }
    }

    fn extend(&mut self, bytes: &[u8]) {
        self.buffer.extend_from_slice(bytes);
    }

    /// Forward every newline-terminated control frame in the buffer.
    ///
    /// Each complete line is classified through the SAME
    /// [`classify_control_line`] seam the plaintext reader uses and forwarded
    /// with `try_send`; the line is drained from the buffer only on a successful
    /// send. An unterminated line that grows past `max_line_bytes` is reported as
    /// a malformed terminal control, mirroring the plaintext reader's
    /// `LineTooLong` handling.
    fn forward_complete_lines(
        &mut self,
        control_tx: &flume::Sender<SessionControl>,
        limits: &Limits,
        subscription_id: &SubscriptionToken,
    ) -> LineFlow {
        loop {
            let Some(newline) = self.buffer.iter().position(|byte| *byte == b'\n') else {
                if self.buffer.len() > limits.max_line_bytes {
                    return match control_tx.try_send(SessionControl::Malformed) {
                        Ok(()) => {
                            self.buffer.clear();
                            LineFlow::Stopped
                        }
                        Err(TrySendError::Full(_)) => LineFlow::Backpressure,
                        Err(TrySendError::Disconnected(_)) => LineFlow::Stopped,
                    };
                }
                return LineFlow::NeedMore;
            };
            let classified =
                classify_control_line(&self.buffer[..=newline], limits, subscription_id);
            let terminal = classified.terminal;
            match control_tx.try_send(classified.control) {
                Ok(()) => {
                    self.buffer.drain(..=newline);
                    if terminal {
                        return LineFlow::Stopped;
                    }
                }
                Err(TrySendError::Full(_)) => return LineFlow::Backpressure,
                Err(TrySendError::Disconnected(_)) => return LineFlow::Stopped,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    //! Drain/accumulator unit tests. The TLS socket glue (handshake, the
    //! non-blocking toggle, `read_tls`/`process_new_packets`) is covered
    //! end-to-end by `tests/tls_subscription.rs`; these pin the pure line
    //! reassembly and lane back-pressure that the drain depends on, without TLS.

    use super::*;

    const ACK_LINE: &[u8] = b"NETBAT/2 SUB_ACK orders.open.v1 1 aabb\n";
    const CANCEL_LINE: &[u8] = b"NETBAT/2 SUB_CANCEL orders.open.v1 client.cancel\n";

    fn token() -> SubscriptionToken {
        SubscriptionToken::new("orders.open.v1", &Limits::default()).expect("token")
    }

    #[test]
    fn forwards_a_complete_ack_without_stopping() {
        // A well-formed, id-matching SUB_ACK is non-terminal: it is forwarded and
        // the drain keeps reading (NeedMore), with the line consumed from the
        // buffer.
        let (tx, rx) = flume::bounded(16);
        let mut acc = ControlAccumulator::new();
        acc.extend(ACK_LINE);
        assert!(matches!(
            acc.forward_complete_lines(&tx, &Limits::default(), &token()),
            LineFlow::NeedMore
        ));
        assert!(matches!(rx.try_recv(), Ok(SessionControl::Ack { .. })));
        assert!(acc.buffer.is_empty(), "the consumed line is drained");
    }

    #[test]
    fn cancel_is_terminal_and_stops() {
        let (tx, rx) = flume::bounded(16);
        let mut acc = ControlAccumulator::new();
        acc.extend(CANCEL_LINE);
        assert!(matches!(
            acc.forward_complete_lines(&tx, &Limits::default(), &token()),
            LineFlow::Stopped
        ));
        assert!(matches!(rx.try_recv(), Ok(SessionControl::Cancel)));
    }

    #[test]
    fn reassembles_a_frame_split_across_extends() {
        // A SUB_CANCEL split mid-line must NOT be forwarded until the newline
        // arrives, then forwarded exactly once as Cancel.
        let (tx, rx) = flume::bounded(16);
        let mut acc = ControlAccumulator::new();
        let split = CANCEL_LINE.len() / 2;
        acc.extend(&CANCEL_LINE[..split]);
        assert!(matches!(
            acc.forward_complete_lines(&tx, &Limits::default(), &token()),
            LineFlow::NeedMore
        ));
        assert!(rx.try_recv().is_err(), "no frame before the line completes");
        acc.extend(&CANCEL_LINE[split..]);
        assert!(matches!(
            acc.forward_complete_lines(&tx, &Limits::default(), &token()),
            LineFlow::Stopped
        ));
        assert!(matches!(rx.try_recv(), Ok(SessionControl::Cancel)));
    }

    #[test]
    fn oversize_unterminated_line_is_malformed_terminal() {
        // A line that grows past the cap without a newline must surface a
        // malformed terminal control, never grow the buffer without bound.
        let limits = Limits::default().with_max_line_bytes(8);
        let (tx, rx) = flume::bounded(16);
        let mut acc = ControlAccumulator::new();
        acc.extend(b"NETBAT/2 SUB_ACK no-newline-here-yet");
        assert!(matches!(
            acc.forward_complete_lines(&tx, &limits, &token()),
            LineFlow::Stopped
        ));
        assert!(matches!(rx.try_recv(), Ok(SessionControl::Malformed)));
        assert!(acc.buffer.is_empty());
    }

    #[test]
    fn full_lane_reports_backpressure_and_keeps_the_line() {
        // With the lane already full, a complete line cannot be forwarded: report
        // Backpressure and retain the line so it is retried after the next poll
        // drains the lane — no frame is dropped.
        let (tx, rx) = flume::bounded(1);
        tx.try_send(SessionControl::Cancel)
            .expect("prefill the lane");
        let mut acc = ControlAccumulator::new();
        acc.extend(ACK_LINE);
        assert!(matches!(
            acc.forward_complete_lines(&tx, &Limits::default(), &token()),
            LineFlow::Backpressure
        ));
        assert_eq!(
            acc.buffer, ACK_LINE,
            "the unsent line is retained for retry"
        );
        // Drain the prefill; the retry now lands the ACK.
        assert!(matches!(rx.try_recv(), Ok(SessionControl::Cancel)));
        assert!(matches!(
            acc.forward_complete_lines(&tx, &Limits::default(), &token()),
            LineFlow::NeedMore
        ));
        assert!(matches!(rx.try_recv(), Ok(SessionControl::Ack { .. })));
    }
}
