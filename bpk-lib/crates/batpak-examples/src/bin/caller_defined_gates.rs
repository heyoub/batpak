//! # caller_defined_gates
//!
//! **Teaches:** gate-based propose -> evaluate -> commit pipeline with
//! `CommitMetadata::validate`.
//!
//! Run: `cargo run -p batpak-examples --bin caller_defined_gates`

use batpak::guard::{Denial, Gate, GateSet};
use batpak::id::EntityIdType;
use batpak::pipeline::{CommitMetadata, Pipeline, Proposal};
use batpak::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize, EventPayload)]
#[batpak(category = 4, type_id = 1)]
struct WriteRequest {
    stream: String,
    bytes: u64,
    tag: String,
}

struct SizeLimitGate {
    max_bytes: u64,
}

impl Gate<WriteRequest> for SizeLimitGate {
    fn name(&self) -> &'static str {
        "size_limit"
    }

    fn evaluate(&self, ctx: &WriteRequest) -> Result<(), Denial> {
        if ctx.bytes > self.max_bytes {
            Err(Denial::new(
                self.name(),
                format!("write size {} exceeds limit {}", ctx.bytes, self.max_bytes),
            )
            .with_code("SIZE_EXCEEDED")
            .with_context("requested", ctx.bytes.to_string())
            .with_context("limit", self.max_bytes.to_string()))
        } else {
            Ok(())
        }
    }

    fn description(&self) -> &'static str {
        "Rejects writes exceeding the configured byte limit"
    }
}

struct TagDenyGate {
    blocked_tags: &'static [&'static str],
}

impl Gate<WriteRequest> for TagDenyGate {
    fn name(&self) -> &'static str {
        "tag_deny"
    }

    fn evaluate(&self, ctx: &WriteRequest) -> Result<(), Denial> {
        if self.blocked_tags.contains(&ctx.tag.as_str()) {
            return Err(Denial::new(self.name(), "tag is blocked")
                .with_code("TAG_BLOCKED")
                .with_context("tag", ctx.tag.clone()));
        }
        Ok(())
    }
}

fn try_write(store: &Store, pipeline: &Pipeline<WriteRequest>, request: &WriteRequest) {
    use std::io::Write;
    let mut out = std::io::stdout().lock();

    let label = format!(
        "{} bytes={} tag={}",
        request.stream, request.bytes, request.tag
    );
    let proposal = Proposal::new(request.clone());

    match pipeline.evaluate(request, proposal) {
        Ok(receipt) => {
            let _ = writeln!(out, "  ACCEPTED: {label}");
            let _ = writeln!(out, "    Gates passed: {:?}", receipt.gates_passed());

            let coord = Coordinate::new(format!("stream:{}", request.stream), "writes:accepted")
                .expect("valid coordinate");

            let result: Result<_, StoreError> = pipeline.commit(receipt, |payload| {
                let r = store.append_typed(&coord, payload)?;
                CommitMetadata::from_append_receipt(&r)
            });

            match result {
                Ok(committed) => {
                    let _ = writeln!(
                        out,
                        "    Committed: event_id={:032x}",
                        committed.event_id().as_u128()
                    );
                }
                Err(error) => {
                    let _ = writeln!(out, "    Commit failed: {error}");
                }
            }
        }
        Err(denial) => {
            let _ = writeln!(out, "  DENIED: {label}");
            let _ = writeln!(out, "    Gate: {}", denial.gate);
            let _ = writeln!(out, "    Code: {}", denial.code);
            let _ = writeln!(out, "    Reason: {}", denial.message);
            for (key, value) in &denial.context {
                let _ = writeln!(out, "    {key}: {value}");
            }
        }
    }
    let _ = writeln!(out);
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Write;
    let mut out = std::io::stdout().lock();

    let dir = tempfile::tempdir()?;
    let store = Store::open(StoreConfig::new(dir.path()))?;

    let mut gates: GateSet<WriteRequest> = GateSet::new();
    gates.push(TagDenyGate {
        blocked_tags: &["blocked"],
    });
    gates.push(SizeLimitGate { max_bytes: 4096 });

    let pipeline = Pipeline::new(gates);

    let _ = writeln!(out, "=== Caller-Defined Gates ===\n");
    drop(out);

    try_write(
        &store,
        &pipeline,
        &WriteRequest {
            stream: "alpha".into(),
            bytes: 1024,
            tag: "normal".into(),
        },
    );

    try_write(
        &store,
        &pipeline,
        &WriteRequest {
            stream: "beta".into(),
            bytes: 512,
            tag: "blocked".into(),
        },
    );

    try_write(
        &store,
        &pipeline,
        &WriteRequest {
            stream: "gamma".into(),
            bytes: 8192,
            tag: "normal".into(),
        },
    );

    Ok(())
}
