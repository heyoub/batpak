//! # Policy Gates — enforcing rules before events are committed
//!
//! A bank transfer system where every transfer must pass through policy gates
//! before it can be committed to the event log. Gates enforce:
//!
//! 1. **Amount limit** — no single transfer over $10,000
//! 2. **Sanctions check** — no transfers to sanctioned entities
//! 3. **Business hours** — transfers only during business hours (simulated)
//!
//! When a gate denies a transfer, you get a structured `Denial` with the gate
//! name, error code, and context — not just a string. The Receipt proves all
//! gates passed; it's unforgeable (sealed type) and consumed exactly once.
//!
//! This is the "propose → evaluate → commit" pipeline in action.
//!
//! Run: `cargo run --example policy_gates`

use batpak::prelude::*;
use serde::Serialize;

const TRANSFER: EventKind = EventKind::custom(4, 1);

#[derive(Clone, Serialize)]
struct TransferRequest {
    from: String,
    to: String,
    amount_cents: u64,
    memo: String,
}

// -- Gate 1: Amount limit --
struct AmountLimitGate {
    max_cents: u64,
}

impl Gate<TransferRequest> for AmountLimitGate {
    fn name(&self) -> &'static str {
        "amount_limit"
    }

    fn evaluate(&self, ctx: &TransferRequest) -> Result<(), Denial> {
        if ctx.amount_cents > self.max_cents {
            Err(Denial::new(
                self.name(),
                format!(
                    "Transfer of ${:.2} exceeds limit of ${:.2}",
                    ctx.amount_cents as f64 / 100.0,
                    self.max_cents as f64 / 100.0,
                ),
            )
            .with_code("AMOUNT_EXCEEDED")
            .with_context("requested", format!("{}", ctx.amount_cents))
            .with_context("limit", format!("{}", self.max_cents)))
        } else {
            Ok(())
        }
    }

    fn description(&self) -> &'static str {
        "Rejects transfers exceeding the configured amount limit"
    }
}

// -- Gate 2: Sanctions check --
struct SanctionsGate {
    blocked_entities: &'static [&'static str],
}

impl Gate<TransferRequest> for SanctionsGate {
    fn name(&self) -> &'static str {
        "sanctions_check"
    }

    fn evaluate(&self, ctx: &TransferRequest) -> Result<(), Denial> {
        for blocked in self.blocked_entities {
            if ctx.to.contains(blocked) || ctx.from.contains(blocked) {
                return Err(Denial::new(self.name(), "Entity is on sanctions list")
                    .with_code("SANCTIONED_ENTITY")
                    .with_context("entity", blocked.to_string()));
            }
        }
        Ok(())
    }
}

// -- Gate 3: Business hours (simulated) --
struct BusinessHoursGate {
    is_business_hours: bool, // In real code, you'd check the clock
}

impl Gate<TransferRequest> for BusinessHoursGate {
    fn name(&self) -> &'static str {
        "business_hours"
    }

    fn evaluate(&self, _ctx: &TransferRequest) -> Result<(), Denial> {
        if self.is_business_hours {
            Ok(())
        } else {
            Err(Denial::new(
                self.name(),
                "Transfers only allowed during business hours (9am-5pm)",
            )
            .with_code("OUTSIDE_HOURS"))
        }
    }
}

fn try_transfer(store: &Store, pipeline: &Pipeline<TransferRequest>, transfer: TransferRequest) {
    let label = format!(
        "${:.2} {} → {}",
        transfer.amount_cents as f64 / 100.0,
        transfer.from,
        transfer.to
    );

    // Step 1: Wrap in a Proposal
    let proposal = Proposal::new(transfer.clone());

    // Step 2: Evaluate through all gates
    match pipeline.evaluate(&transfer, proposal) {
        Ok(receipt) => {
            // Step 3: Receipt proves all gates passed — commit it
            println!("  APPROVED: {}", label);
            println!("    Gates passed: {:?}", receipt.gates_passed());

            let coord =
                Coordinate::new(&format!("transfer:{}", transfer.from), "banking:transfers")
                    .expect("valid coordinate");

            // Commit consumes the receipt (can't reuse it)
            let result: Result<_, StoreError> = pipeline.commit(receipt, |payload| {
                let r = store.append(&coord, TRANSFER, &payload)?;
                Ok(Committed {
                    payload,
                    event_id: r.event_id,
                    sequence: r.sequence,
                    hash: [0u8; 32],
                })
            });

            match result {
                Ok(committed) => println!("    Committed: event_id={:032x}", committed.event_id),
                Err(e) => println!("    Commit failed: {e}"),
            }
        }
        Err(denial) => {
            println!("  DENIED: {}", label);
            println!("    Gate: {}", denial.gate);
            println!("    Code: {}", denial.code);
            println!("    Reason: {}", denial.message);
            if !denial.context.is_empty() {
                for (k, v) in &denial.context {
                    println!("    {}: {}", k, v);
                }
            }
        }
    }
    println!();
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let store = Store::open(StoreConfig::new(dir.path()))?;

    // Build the gate set — gates are evaluated in order, fail-fast
    let mut gates: GateSet<TransferRequest> = GateSet::new();
    gates.push(SanctionsGate {
        blocked_entities: &["evil-corp", "shady-llc"],
    });
    gates.push(AmountLimitGate {
        max_cents: 1_000_000, // $10,000
    });
    gates.push(BusinessHoursGate {
        is_business_hours: true,
    });

    let pipeline = Pipeline::new(gates);

    println!("=== Bank Transfer Policy Gates ===\n");
    println!("Gates: sanctions_check → amount_limit → business_hours\n");

    // Transfer 1: Should pass all gates
    try_transfer(
        &store,
        &pipeline,
        TransferRequest {
            from: "acme-inc".into(),
            to: "widgets-ltd".into(),
            amount_cents: 50000, // $500
            memo: "Invoice #1234".into(),
        },
    );

    // Transfer 2: Should fail sanctions check (fail-fast, won't reach amount gate)
    try_transfer(
        &store,
        &pipeline,
        TransferRequest {
            from: "acme-inc".into(),
            to: "evil-corp".into(),
            amount_cents: 100, // $1
            memo: "Totally legitimate".into(),
        },
    );

    // Transfer 3: Should fail amount limit
    try_transfer(
        &store,
        &pipeline,
        TransferRequest {
            from: "acme-inc".into(),
            to: "mega-corp".into(),
            amount_cents: 5_000_000, // $50,000
            memo: "Big purchase".into(),
        },
    );

    // -- Show what made it into the event log --
    println!("--- Committed Events ---");
    let entries = store.query(&Region::scope("banking:transfers"));
    println!(
        "  {} transfer(s) committed (2 denied, never stored)\n",
        entries.len()
    );
    for entry in &entries {
        let stored = store.get(entry.event_id)?;
        println!("  {} → {}", entry.coord, stored.event.payload);
    }

    // -- Demonstrate evaluate_all (collect ALL denials, don't fail-fast) --
    println!("\n--- evaluate_all: collect all denials at once ---");
    let mut all_gates: GateSet<TransferRequest> = GateSet::new();
    all_gates.push(SanctionsGate {
        blocked_entities: &["evil-corp"],
    });
    all_gates.push(AmountLimitGate {
        max_cents: 1_000_000,
    });
    all_gates.push(BusinessHoursGate {
        is_business_hours: false, // outside hours this time
    });

    let bad_transfer = TransferRequest {
        from: "evil-corp".into(),
        to: "widgets-ltd".into(),
        amount_cents: 5_000_000,
        memo: "Everything wrong".into(),
    };

    let all_denials = all_gates.evaluate_all(&bad_transfer);
    println!("  {} denials for one transfer:", all_denials.len());
    for d in &all_denials {
        println!("    [{}] {} (code: {})", d.gate, d.message, d.code);
    }

    store.close()?;
    println!("\nGates are pure predicates. No I/O, no side effects, composable.");

    Ok(())
}
