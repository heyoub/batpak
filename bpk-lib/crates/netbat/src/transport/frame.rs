use super::error::NetbatError;
use super::hex::{decode_hex, encode_hex_into};
use super::limits::{Limits, CALL_VERB, LINE_PROTOCOL_VERSION, PROTOCOL_PREFIX};

/// Decoded request frame for netbat's blocking line protocol.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RequestFrame {
    operation: String,
    input: Vec<u8>,
}

impl RequestFrame {
    /// Build a request frame from an operation name and input bytes.
    #[must_use]
    pub fn new(operation: impl Into<String>, input: impl Into<Vec<u8>>) -> Self {
        Self {
            operation: operation.into(),
            input: input.into(),
        }
    }

    /// Requested syncbat operation name.
    #[must_use]
    pub fn operation(&self) -> &str {
        &self.operation
    }

    /// Decoded input bytes.
    #[must_use]
    pub fn input(&self) -> &[u8] {
        &self.input
    }

    /// Consume this request frame and return its parts.
    #[must_use]
    pub fn into_parts(self) -> (String, Vec<u8>) {
        (self.operation, self.input)
    }
}

/// Encoded runtime output returned through a netbat transport frame.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResponseFrame {
    output: Vec<u8>,
}

impl ResponseFrame {
    /// Build a response frame from output bytes.
    #[must_use]
    pub fn new(output: impl Into<Vec<u8>>) -> Self {
        Self {
            output: output.into(),
        }
    }

    /// Handler output bytes.
    #[must_use]
    pub fn output(&self) -> &[u8] {
        &self.output
    }

    /// Consume this response and return output bytes.
    #[must_use]
    pub fn into_output(self) -> Vec<u8> {
        self.output
    }
}

/// Decode one netbat line-protocol request.
///
/// Format:
///
/// ```text
/// NETBAT/1 CALL <operation-name> <hex-input>\n
/// ```
///
/// The legacy first-rung frame is still accepted for callers that already
/// speak it:
///
/// ```text
/// CALL <operation-name> <hex-input>\n
/// ```
///
/// `operation-name` must be non-empty ASCII graphic bytes with no whitespace.
/// Input bytes are hex-encoded to keep the transport line deterministic and
/// byte-safe without introducing a protocol dependency.
///
/// # Errors
/// Returns [`NetbatError`] when the frame is malformed or exceeds limits.
pub fn decode_line(line: &[u8], limits: &Limits) -> Result<RequestFrame, NetbatError> {
    if line.len() > limits.max_line_bytes {
        return Err(NetbatError::LineTooLong {
            max: limits.max_line_bytes,
        });
    }

    let line = strip_line_ending(line);
    if line.is_empty() {
        return Err(NetbatError::MalformedRequest {
            reason: "empty line",
        });
    }

    let mut parts = line.split(|byte| *byte == b' ');
    let first = parts.next().ok_or(NetbatError::MalformedRequest {
        reason: "missing verb",
    })?;
    let (verb, operation, input) = if first.starts_with(PROTOCOL_PREFIX.as_bytes()) {
        validate_protocol_version(first)?;
        let verb = parts.next().ok_or(NetbatError::MalformedRequest {
            reason: "missing verb",
        })?;
        let operation = parts.next().ok_or(NetbatError::MalformedRequest {
            reason: "missing operation",
        })?;
        let input = parts.next().ok_or(NetbatError::MalformedRequest {
            reason: "missing input",
        })?;
        (verb, operation, input)
    } else {
        let operation = parts.next().ok_or(NetbatError::MalformedRequest {
            reason: "missing operation",
        })?;
        let input = parts.next().ok_or(NetbatError::MalformedRequest {
            reason: "missing input",
        })?;
        (first, operation, input)
    };

    if parts.next().is_some() {
        return Err(NetbatError::MalformedRequest {
            reason: "too many fields",
        });
    }
    if verb != CALL_VERB.as_bytes() {
        return Err(NetbatError::MalformedRequest {
            reason: "unsupported verb",
        });
    }
    validate_operation_name(operation, limits)?;

    let input = decode_hex(input, limits.max_input_bytes)?;
    let operation = std::str::from_utf8(operation)
        .map_err(|_| NetbatError::MalformedRequest {
            reason: "operation is not utf-8",
        })?
        .to_owned();

    Ok(RequestFrame::new(operation, input))
}

/// Encode a stable versioned request line.
///
/// Format:
///
/// ```text
/// NETBAT/1 CALL <operation-name> <hex-input>\n
/// ```
///
/// This helper intentionally does not validate the operation name. The decoder
/// remains the validation boundary so invalid names round-trip into the same
/// [`NetbatError::MalformedRequest`] shape as hand-written frames.
#[must_use]
pub fn encode_request(operation: &str, input: &[u8]) -> Vec<u8> {
    let mut line = Vec::with_capacity(
        LINE_PROTOCOL_VERSION.len()
            + 1
            + CALL_VERB.len()
            + 1
            + operation.len()
            + 1
            + input.len() * 2
            + 1,
    );
    line.extend_from_slice(LINE_PROTOCOL_VERSION.as_bytes());
    line.push(b' ');
    line.extend_from_slice(CALL_VERB.as_bytes());
    line.push(b' ');
    line.extend_from_slice(operation.as_bytes());
    line.push(b' ');
    encode_hex_into(input, &mut line);
    line.push(b'\n');
    line
}

/// Encode a stable response line.
///
/// Success format:
///
/// ```text
/// OK <hex-output>\n
/// ```
///
/// Error format:
///
/// ```text
/// ERR <code> <hex-message>\n
/// ```
#[must_use]
pub fn encode_response(result: Result<&[u8], &NetbatError>) -> Vec<u8> {
    match result {
        Ok(output) => {
            let mut response = b"OK ".to_vec();
            encode_hex_into(output, &mut response);
            response.push(b'\n');
            response
        }
        Err(error) => {
            let mut response = format!("ERR {} ", error.code()).into_bytes();
            encode_hex_into(error.to_string().as_bytes(), &mut response);
            response.push(b'\n');
            response
        }
    }
}

/// Dispatch a decoded request frame through syncbat.
///
/// # Errors
/// Returns [`NetbatError`] when syncbat rejects the checkout or output exceeds
/// configured transport limits.
#[tracing::instrument(
    name = "netbat.dispatch_frame",
    skip_all,
    fields(
        operation = %frame.operation(),
        input_bytes = frame.input().len(),
        output_bytes = tracing::field::Empty,
    ),
)]
pub fn dispatch_frame(
    core: &mut syncbat::Core,
    frame: RequestFrame,
    limits: &Limits,
) -> Result<ResponseFrame, NetbatError> {
    validate_request_frame(&frame, limits)?;
    let (operation, input) = frame.into_parts();
    let result = core.checkout_frame(syncbat::CheckoutFrame::new(operation, input))?;
    let output = result.into_output();
    if output.len() > limits.max_output_bytes {
        tracing::warn!(
            output_bytes = output.len(),
            max = limits.max_output_bytes,
            "output exceeded transport limit",
        );
        return Err(NetbatError::OutputTooLarge {
            max: limits.max_output_bytes,
        });
    }
    tracing::Span::current().record("output_bytes", output.len());
    Ok(ResponseFrame::new(output))
}

fn strip_line_ending(line: &[u8]) -> &[u8] {
    line.strip_suffix(b"\n")
        .unwrap_or(line)
        .strip_suffix(b"\r")
        .unwrap_or_else(|| line.strip_suffix(b"\n").unwrap_or(line))
}

fn validate_protocol_version(version: &[u8]) -> Result<(), NetbatError> {
    if version == LINE_PROTOCOL_VERSION.as_bytes() {
        return Ok(());
    }
    Err(NetbatError::UnsupportedProtocolVersion {
        version: String::from_utf8_lossy(version).into_owned(),
    })
}

fn validate_operation_name(operation: &[u8], limits: &Limits) -> Result<(), NetbatError> {
    // The transport bound is a *transport* concern, separate from the
    // type-level [`syncbat::OperationName::MAX_BYTES`]. Apply it first so
    // configurable downsizing keeps surfacing as `OperationNameTooLong`
    // rather than getting reshaped into a `MalformedRequest`.
    if operation.len() > limits.max_operation_name_bytes {
        return Err(NetbatError::OperationNameTooLong {
            max: limits.max_operation_name_bytes,
        });
    }
    // Non-UTF-8 bytes are categorically outside the ASCII operation-name
    // grammar; surface them under the same "operation has invalid bytes"
    // token that the byte-level grammar check would have produced before
    // the [`OperationName`] consolidation.
    let s = std::str::from_utf8(operation).map_err(|_| NetbatError::MalformedRequest {
        reason: "operation has invalid bytes",
    })?;
    syncbat::OperationName::new(s)
        .map(|_| ())
        .map_err(|error| match error {
            syncbat::OperationNameError::Empty => NetbatError::MalformedRequest {
                reason: "empty operation",
            },
            // Reachable only when `limits.max_operation_name_bytes` is configured
            // *above* the type-level `MAX_BYTES`; preserves the wire token.
            syncbat::OperationNameError::TooLong { .. } => NetbatError::OperationNameTooLong {
                max: syncbat::OperationName::MAX_BYTES,
            },
            syncbat::OperationNameError::IllegalCharacter { .. } => NetbatError::MalformedRequest {
                reason: "operation has invalid bytes",
            },
            syncbat::OperationNameError::LeadingOrTrailingDot
            | syncbat::OperationNameError::ConsecutiveDots => NetbatError::MalformedRequest {
                reason: "operation dot segments must be non-empty",
            },
            // `OperationNameError` is `#[non_exhaustive]`; any post-1.0 variant
            // surfaces under the existing malformed-request token until netbat
            // grows a more specific mapping.
            _ => NetbatError::MalformedRequest {
                reason: "operation has invalid bytes",
            },
        })
}

fn validate_request_frame(frame: &RequestFrame, limits: &Limits) -> Result<(), NetbatError> {
    validate_operation_name(frame.operation().as_bytes(), limits)?;
    if frame.input().len() > limits.max_input_bytes {
        return Err(NetbatError::InputTooLarge {
            max: limits.max_input_bytes,
        });
    }
    Ok(())
}
