use crate::store::stats::HlcPoint;

/// Typed internal invariant violation.
#[derive(Debug)]
#[non_exhaustive]
pub enum StoreInvariant {
    /// A later close event carried an older HLC point in log order.
    CloseHlcRegression {
        /// Previous close HLC in log order.
        previous: HlcPoint,
        /// Later close HLC in log order.
        later: HlcPoint,
    },
    /// The lifecycle open HLC candidate was older than recovered store state.
    BootstrapHlcOutOfOrder {
        /// Candidate open HLC.
        open_hlc: HlcPoint,
        /// Highest HLC recovered from the index.
        max_recovered_hlc: HlcPoint,
        /// Latest close HLC recovered from lifecycle events.
        last_close_hlc: HlcPoint,
    },
    /// Converting open HLC wall milliseconds to microseconds overflowed.
    OpenHlcWallMsOverflow {
        /// HLC wall milliseconds.
        wall_ms: u64,
    },
    /// Converted open HLC timestamp exceeded the signed timestamp range.
    OpenHlcTimestampOutOfRange {
        /// HLC wall milliseconds.
        wall_ms: u64,
    },
    /// The SYSTEM_OPEN_COMPLETED receipt was not visible in the rebuilt index.
    OpenReceiptNotIndexed {
        /// Receipt event id.
        event_id: u128,
    },
    /// A durability gate could not find the append receipt in the index.
    GateReceiptNotIndexed {
        /// Receipt event id.
        event_id: u128,
    },
    /// Prepared batch staging ended with a different item count than declared.
    PreparedBatchItemCountDrift {
        /// Declared item count.
        expected: usize,
        /// Actual staged item count.
        actual: usize,
    },
}

impl std::fmt::Display for StoreInvariant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CloseHlcRegression { previous, later } => write!(
                f,
                "SYSTEM_CLOSE_COMPLETED HLC regressed in log order: previous {previous:?}, later {later:?}"
            ),
            Self::BootstrapHlcOutOfOrder {
                open_hlc,
                max_recovered_hlc,
                last_close_hlc,
            } => write!(
                f,
                "open_hlc {open_hlc:?} must be >= max_recovered_hlc {max_recovered_hlc:?} and last_close_hlc {last_close_hlc:?}"
            ),
            Self::OpenHlcWallMsOverflow { wall_ms } => {
                write!(f, "open_hlc wall_ms {wall_ms} overflows timestamp_us")
            }
            Self::OpenHlcTimestampOutOfRange { wall_ms } => write!(
                f,
                "open_hlc wall_ms {wall_ms} exceeds i64 timestamp_us range"
            ),
            Self::OpenReceiptNotIndexed { event_id } => write!(
                f,
                "SYSTEM_OPEN_COMPLETED receipt {event_id:032x} was not visible in the rebuilt index"
            ),
            Self::GateReceiptNotIndexed { event_id } => write!(
                f,
                "append receipt {event_id:032x} was not visible for durability gate lookup"
            ),
            Self::PreparedBatchItemCountDrift { expected, actual } => write!(
                f,
                "prepared batch item count changed during staging: expected {expected}, got {actual}"
            ),
        }
    }
}
