//! The bvisor admission kernel — the bounded NC¹ decision object.
//!
//! Three computations, per the kernel plan §1:
//! - `C : (Spec, Profile) -> AdmissionProgram` — the compiler (total, bounded,
//!   deterministic; NOT claimed NC¹). Lands in a later step.
//! - `E : (AdmissionProgram, x) -> decision` — the [`evaluate`]or. The NC¹
//!   computation: a single forward pass over a logarithmic-depth circuit.
//! - `X` — execution/supervision, elsewhere.
//!
//! Submodules:
//! - [`program`] — the FROZEN IR: closed [`NodeOp`] vocabulary, canonical
//!   topological encoding, structural limits, bit-level depth recurrence, proof
//!   certificate.
//! - [`eval`] — the evaluator `E`: pure, total, FAIL-CLOSED on any malformed or
//!   ill-typed program (it never panics, so a hostile/random program is a typed
//!   error, not a crash — a precondition for the equivalence/fuzz harness).
//!
//! The independent validator (step 3) and the compiler `C` (step 2 proper) land
//! as further submodules; they consume the artifacts frozen in [`program`].

mod eval;
mod limits;
mod program;
mod validate;

pub use eval::{evaluate, Decision, EvalError, Lane};
pub use limits::{LimitViolation, ProgramLimits, FROZEN_LIMITS};
pub use program::{
    AdmissionProgram, CertNode, CompareRel, InputDecl, InputSlot, LookupTable, Node, NodeId,
    NodeOp, Outputs, ProgramCertificate, ProgramError, Width, ADMISSION_PROGRAM_SCHEMA_VERSION,
    MAX_WIDTH,
};
pub use validate::{
    decode_validated, validate, verify_certificate, ValidationError, MAX_LOOKUP_ENTRIES,
};
