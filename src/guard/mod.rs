/// Gate denial types returned when a gate rejects a proposal.
pub mod denial;
/// Receipt types proving a proposal passed all gates.
pub mod receipt;

pub use denial::{Denial, DenialPayload, GateEvaluation, GateId, GateIdError, Verdict};
pub use receipt::Receipt;

/// `Gate<Ctx>`: a predicate that evaluates a context and either permits or denies.
/// Gates are PREDICATES, not transformers. No I/O, no mutation, pure.
/// Ctx is product-defined. Library is generic over it.
pub trait Gate<Ctx>: Send + Sync {
    /// Returns a unique, human-readable identifier for this gate.
    fn name(&self) -> &'static str;
    /// Evaluates the context; returns `Ok(())` to permit or a `Denial` to reject.
    ///
    /// # Errors
    /// Returns a [`Denial`] if the gate rejects the operation.
    fn evaluate(&self, ctx: &Ctx) -> Result<(), Denial>;
    /// Returns an optional description of what this gate checks.
    fn description(&self) -> &'static str {
        ""
    }
}

/// `GateSet<Ctx>`: ordered collection of gates. Fail-fast by default.
pub struct GateSet<Ctx> {
    /// Ordered list of boxed gate predicates to evaluate in sequence.
    gates: Vec<Box<dyn Gate<Ctx>>>,
}

impl<Ctx> GateSet<Ctx> {
    /// Creates an empty `GateSet`.
    pub fn new() -> Self {
        Self { gates: vec![] }
    }

    /// Appends a gate to the end of the set.
    pub fn push(&mut self, gate: impl Gate<Ctx> + 'static) {
        self.gates.push(Box::new(gate));
    }

    /// Fail-fast evaluation. First denial stops.
    /// Returns `Receipt<T>` wrapping the proposal payload on success.
    ///
    /// # Errors
    /// Returns the first `Denial` produced by any gate in the set.
    pub fn evaluate<T>(
        &self,
        ctx: &Ctx,
        proposal: crate::pipeline::Proposal<T>,
    ) -> Result<Receipt<T>, Denial> {
        for gate in &self.gates {
            gate.evaluate(ctx)?;
        }
        let names: Vec<&'static str> = self.gates.iter().map(|g| g.name()).collect();
        Ok(Receipt::new(proposal.0, names))
    }

    /// Evaluate ALL gates (no fail-fast). For observability — collect all denials.
    /// Gates that panic are caught and surfaced as `Denial` with code `GATE_DEFECT`.
    pub fn evaluate_all(&self, ctx: &Ctx) -> Vec<Denial> {
        self.gates
            .iter()
            .filter_map(|g| {
                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| g.evaluate(ctx))) {
                    Ok(Ok(())) => None,
                    Ok(Err(denial)) => Some(denial),
                    Err(panic_payload) => {
                        let msg = if let Some(s) = panic_payload.downcast_ref::<&str>() {
                            (*s).to_string()
                        } else if let Some(s) = panic_payload.downcast_ref::<String>() {
                            s.clone()
                        } else {
                            "unknown panic".to_string()
                        };
                        Some(Denial::new(g.name(), msg).with_code("GATE_DEFECT"))
                    }
                }
            })
            .collect()
    }

    /// Build a persistable denial trace from a fail-fast denial result.
    pub fn trace_denial(
        &self,
        failing: &Denial,
        proposed_kind: crate::event::EventKind,
        proposed_content_hash: Option<[u8; 32]>,
        pipeline_id: Option<String>,
    ) -> DenialPayload {
        let mut matched_denier = false;
        let mut evaluations = Vec::with_capacity(self.gates.len().max(1));

        for gate in &self.gates {
            let gate_id = GateId::from_name(gate.name());
            let verdict = if matched_denier {
                Verdict::Skipped
            } else if gate.name() == failing.gate {
                matched_denier = true;
                Verdict::Deny {
                    code: failing.code.clone(),
                    message: failing.message.clone(),
                    context: failing.context.clone(),
                }
            } else {
                Verdict::Permit
            };
            evaluations.push(GateEvaluation::new(gate_id, verdict, None));
        }

        if !matched_denier {
            evaluations.push(GateEvaluation::new(
                GateId::from_name(failing.gate),
                Verdict::Deny {
                    code: failing.code.clone(),
                    message: failing.message.clone(),
                    context: failing.context.clone(),
                },
                None,
            ));
        }

        DenialPayload::new(
            evaluations,
            pipeline_id,
            proposed_kind,
            proposed_content_hash,
        )
    }

    /// Returns the number of gates in the set.
    pub fn len(&self) -> usize {
        self.gates.len()
    }
    /// Returns `true` if the set contains no gates.
    pub fn is_empty(&self) -> bool {
        self.gates.is_empty()
    }
    /// Returns the names of all gates in order.
    pub fn names(&self) -> Vec<&'static str> {
        self.gates.iter().map(|g| g.name()).collect()
    }
}

impl<Ctx> Default for GateSet<Ctx> {
    fn default() -> Self {
        Self::new()
    }
}
