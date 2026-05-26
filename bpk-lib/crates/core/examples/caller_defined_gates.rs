// justifies: INV-EXAMPLES-OBSERVABLE-OUTPUT; caller-defined-gates example in examples/caller_defined_gates.rs shows the propose-evaluate-commit flow via println, and passes gate-shaped arguments by value/borrow as the gate API expects from a teaching fixture.
#![allow(
    clippy::print_stdout,
    clippy::needless_pass_by_value,
    clippy::needless_borrows_for_generic_args
)]
//! # caller_defined_gates
//!
//! **Teaches:** gate-based propose -> evaluate -> commit pipeline with
//! `CommitMetadata::validate`.
//!
//! Run: `cargo run --example caller_defined_gates`

use batpak::guard::{Denial, Gate, GateSet};
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

fn try_write(store: &Store, pipeline: &Pipeline<WriteRequest>, request: WriteRequest) {
    let label = format!(
        "{} bytes={} tag={}",
        request.stream, request.bytes, request.tag
    );
    let proposal = Proposal::new(request.clone());

    match pipeline.evaluate(&request, proposal) {
        Ok(receipt) => {
            println!("  ACCEPTED: {label}");
            println!("    Gates passed: {:?}", receipt.gates_passed());

            let coord = Coordinate::new(&format!("stream:{}", request.stream), "writes:accepted")
                .expect("valid coordinate");

            let result: Result<_, StoreError> = pipeline.commit(receipt, |payload| {
                let r = store.append_typed(&coord, payload)?;
                CommitMetadata::from_append_receipt(&r)
            });

            match result {
                Ok(committed) => println!("    Committed: event_id={:032x}", committed.event_id()),
                Err(error) => println!("    Commit failed: {error}"),
            }
        }
        Err(denial) => {
            println!("  DENIED: {label}");
            println!("    Gate: {}", denial.gate);
            println!("    Code: {}", denial.code);
            println!("    Reason: {}", denial.message);
            for (key, value) in &denial.context {
                println!("    {key}: {value}");
            }
        }
    }
    println!();
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let store = Store::open(StoreConfig::new(dir.path()))?;

    let mut gates: GateSet<WriteRequest> = GateSet::new();
    gates.push(TagDenyGate {
        blocked_tags: &["blocked"],
    });
    gates.push(SizeLimitGate { max_bytes: 4096 });

    let pipeline = Pipeline::new(gates);

    println!("=== Caller-Defined Gates ===\n");

    try_write(
        &store,
        &pipeline,
        WriteRequest {
            stream: "alpha".into(),
            bytes: 1024,
            tag: "normal".into(),
        },
    );

    try_write(
        &store,
        &pipeline,
        WriteRequest {
            stream: "beta".into(),
            bytes: 512,
            tag: "blocked".into(),
        },
    );

    try_write(
        &store,
        &pipeline,
        WriteRequest {
            stream: "gamma".into(),
            bytes: 8192,
            tag: "normal".into(),
        },
    );

    Ok(())
}
