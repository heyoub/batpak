use crate::store::segment::scan::FrameScanTailPolicy;
use crate::store::StoreError;
use std::io::{Error, ErrorKind};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PayloadReadFailure {
    RecoverTornTail,
}

pub(super) fn classify_payload_read_error(
    segment_id: u64,
    error: Error,
    tail_policy: FrameScanTailPolicy,
) -> Result<PayloadReadFailure, StoreError> {
    if error.kind() == ErrorKind::UnexpectedEof {
        if tail_policy.can_recover_torn_tail() {
            Ok(PayloadReadFailure::RecoverTornTail)
        } else {
            Err(StoreError::corrupt_segment_with_detail(
                segment_id,
                "frame payload ended before requested length",
            ))
        }
    } else {
        Err(StoreError::Io(error))
    }
}
