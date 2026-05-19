/// Default maximum request line size accepted by the line transport.
pub const DEFAULT_MAX_LINE_BYTES: usize = 64 * 1024;
/// Default maximum operation name size accepted by the line transport.
pub const DEFAULT_MAX_OPERATION_NAME_BYTES: usize = syncbat::MAX_OPERATION_NAME_BYTES;
/// Default maximum decoded input size accepted by the line transport.
pub const DEFAULT_MAX_INPUT_BYTES: usize = 32 * 1024;
/// Default maximum handler output size encoded into a response frame.
pub const DEFAULT_MAX_OUTPUT_BYTES: usize = 32 * 1024;

macro_rules! protocol_prefix {
    () => {
        "NETBAT/"
    };
}

/// Prefix used by every versioned netbat line-protocol token.
pub const PROTOCOL_PREFIX: &str = protocol_prefix!();
/// Current version token accepted by netbat's versioned line protocol.
pub const LINE_PROTOCOL_VERSION: &str = concat!(protocol_prefix!(), "1");
/// Request verb used by netbat's line protocol.
pub const CALL_VERB: &str = "CALL";

/// Bounded transport limits for netbat's blocking line protocol.
///
/// `#[non_exhaustive]` so adding a new bound (e.g. `max_header_bytes`
/// once a NETBAT/2 header frame exists) is not a SemVer break.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct Limits {
    /// Maximum bytes read before a newline terminator is required.
    pub max_line_bytes: usize,
    /// Maximum bytes allowed in the operation name token.
    pub max_operation_name_bytes: usize,
    /// Maximum decoded input bytes accepted by dispatch.
    pub max_input_bytes: usize,
    /// Maximum output bytes encoded into a response frame.
    pub max_output_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_line_bytes: DEFAULT_MAX_LINE_BYTES,
            max_operation_name_bytes: DEFAULT_MAX_OPERATION_NAME_BYTES,
            max_input_bytes: DEFAULT_MAX_INPUT_BYTES,
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
        }
    }
}

impl Limits {
    /// Construct the default limits. Equivalent to [`Limits::default`]
    /// but available in `const` contexts up to MSRV permits.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Override [`Limits::max_line_bytes`].
    #[must_use]
    pub const fn with_max_line_bytes(mut self, value: usize) -> Self {
        self.max_line_bytes = value;
        self
    }

    /// Override [`Limits::max_operation_name_bytes`].
    #[must_use]
    pub const fn with_max_operation_name_bytes(mut self, value: usize) -> Self {
        self.max_operation_name_bytes = value;
        self
    }

    /// Override [`Limits::max_input_bytes`].
    #[must_use]
    pub const fn with_max_input_bytes(mut self, value: usize) -> Self {
        self.max_input_bytes = value;
        self
    }

    /// Override [`Limits::max_output_bytes`].
    #[must_use]
    pub const fn with_max_output_bytes(mut self, value: usize) -> Self {
        self.max_output_bytes = value;
        self
    }
}

impl IoTimeouts {
    /// Set the read-timeout hint.
    #[must_use]
    pub const fn with_read(mut self, value: Option<std::time::Duration>) -> Self {
        self.read = value;
        self
    }

    /// Set the write-timeout hint.
    #[must_use]
    pub const fn with_write(mut self, value: Option<std::time::Duration>) -> Self {
        self.write = value;
        self
    }
}

/// Optional read/write timeout hints for listener owners.
///
/// The generic [`crate::serve_stream`] helper works with any [`std::io::Read`] + [`std::io::Write`]
/// value and cannot apply timeouts itself. Listener owners that use
/// `std::net::TcpStream` can apply these values before passing the stream in.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub struct IoTimeouts {
    /// Read timeout hint.
    pub read: Option<std::time::Duration>,
    /// Write timeout hint.
    pub write: Option<std::time::Duration>,
}
