use super::error::NetbatError;

/// Decode a lowercase or uppercase hexadecimal byte string with a decoded-size limit.
///
/// # Errors
/// Returns [`NetbatError`] when the hex string has odd length, contains a
/// non-hex byte, or decodes past `max_input_bytes`.
pub fn decode_hex(input: &[u8], max_input_bytes: usize) -> Result<Vec<u8>, NetbatError> {
    if !input.len().is_multiple_of(2) {
        return Err(NetbatError::MalformedRequest {
            reason: "hex input has odd length",
        });
    }
    if input.len() / 2 > max_input_bytes {
        return Err(NetbatError::InputTooLarge {
            max: max_input_bytes,
        });
    }

    let mut output = Vec::with_capacity(input.len() / 2);
    for pair in input.chunks_exact(2) {
        let high = hex_value(pair[0])?;
        let low = hex_value(pair[1])?;
        output.push((high << 4) | low);
    }
    Ok(output)
}

fn hex_value(byte: u8) -> Result<u8, NetbatError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(NetbatError::MalformedRequest {
            reason: "input is not hex",
        }),
    }
}

/// Append lowercase hexadecimal encoding of `bytes` into `output`.
pub fn encode_hex_into(bytes: &[u8], output: &mut Vec<u8>) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    output.reserve(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize]);
        output.push(HEX[(byte & 0x0f) as usize]);
    }
}

/// Encode `bytes` as a lowercase hexadecimal byte string and return the
/// owned buffer.
#[must_use]
pub fn encode_hex(bytes: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(bytes.len() * 2);
    encode_hex_into(bytes, &mut output);
    output
}

/// Encode `bytes` as a lowercase hexadecimal [`String`].
///
/// Convenience wrapper around [`encode_hex`] for callers (such as
/// substrate operations that carry hex on the wire as msgpack strings)
/// that need an owned `String` rather than `Vec<u8>`. The conversion
/// is allocation-free past [`encode_hex`] because every byte produced
/// by the lowercase-hex encoder is ASCII.
#[must_use]
pub fn encode_hex_str(bytes: &[u8]) -> String {
    let buf = encode_hex(bytes);
    // SAFETY: encode_hex_into emits only ASCII bytes from the
    // 0123456789abcdef alphabet.
    String::from_utf8(buf).expect("lowercase-hex encoder produces ASCII")
}

/// Decode a lowercase or uppercase hexadecimal `&str`.
///
/// Convenience wrapper around [`decode_hex`] without the input-size
/// guard, for callers that already trust the source. Use [`decode_hex`]
/// directly when receiving bytes from an untrusted transport that
/// needs a bound.
///
/// # Errors
/// Returns [`NetbatError::MalformedRequest`] when the input has odd
/// length or contains a non-hex byte.
pub fn decode_hex_str(input: &str) -> Result<Vec<u8>, NetbatError> {
    decode_hex(input.as_bytes(), usize::MAX)
}
