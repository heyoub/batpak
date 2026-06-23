//! The independent structural validator (kernel plan §3) — the trust sink.
//!
//! [`super::program::AdmissionProgram::new`] guards programs built in-process, but
//! serde `Deserialize` bypasses it: a program decoded from untrusted bytes can
//! violate every invariant. This module is the **second wall** (parse-don't-validate
//! for hostile bytes): it independently re-derives and checks everything before a
//! program may be trusted —
//!
//! - schema version is the frozen one;
//! - every lane width is `1 ..= MAX_WIDTH`;
//! - operand fan-in matches each op's frozen arity;
//! - every operand references a strictly-earlier, in-range node (canonical, acyclic);
//! - operand/result widths satisfy each op's typing;
//! - constants, input slots, and lookup tables are well-formed and bounded;
//! - the declared outputs reference valid predicate / refusal lanes;
//! - the program is within [`ProgramLimits`] (polynomial nodes, log depth);
//! - (for the byte entry point) re-encoding reproduces the input exactly.
//!
//! On success it returns the [`ProgramCertificate`] it independently computed —
//! the artifact the compiler `C` also emits, here re-derived rather than trusted.
//! Being the trust sink, the validator must be COMPLETE: its red fixtures prove it
//! rejects each malformed class.

use super::limits::{LimitViolation, ProgramLimits};
use super::program::{
    AdmissionProgram, InputDecl, InputSlot, LookupTable, Node, NodeId, NodeOp, Outputs,
    ProgramCertificate, Width, ADMISSION_PROGRAM_SCHEMA_VERSION, MAX_WIDTH,
};

/// The frozen upper bound on a [`LookupTable`]'s entry count — keeps a bounded
/// lookup actually bounded regardless of key width.
pub const MAX_LOOKUP_ENTRIES: usize = 4096;

/// Why a program failed validation. The validator fails closed on the FIRST fault;
/// each variant names a malformed class its red fixtures prove it catches.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ValidationError {
    /// The program declares a schema version other than the frozen one.
    SchemaVersion {
        /// The declared version.
        found: u16,
    },
    /// The node count is not `u32`-indexable.
    TooLarge,
    /// A declared input lane width is outside `1 ..= MAX_WIDTH`.
    InputWidthOutOfRange {
        /// The input slot.
        slot: usize,
        /// The offending width.
        width: u16,
    },
    /// A node's output width is outside `1 ..= MAX_WIDTH`.
    NodeWidthOutOfRange {
        /// The node index.
        at: u32,
        /// The offending width.
        width: u16,
    },
    /// A node's fan-in does not match its op's frozen arity.
    Arity {
        /// The node index.
        at: u32,
        /// The required arity.
        expected: usize,
        /// The arity found.
        found: usize,
    },
    /// An operand references a node id outside the array.
    OperandOutOfRange {
        /// The node index.
        at: u32,
        /// The out-of-range operand id.
        operand: u32,
    },
    /// An operand references the node itself or a later node.
    NonCanonicalEdge {
        /// The node index.
        at: u32,
        /// The forward/self operand id.
        operand: u32,
    },
    /// Operands that must share (or relate by) a width do not.
    OperandTyping {
        /// The node index.
        at: u32,
        /// What the typing rule requires.
        reason: &'static str,
    },
    /// A node's declared output width disagrees with what its op produces.
    OutputWidth {
        /// The node index.
        at: u32,
        /// What the rule requires.
        reason: &'static str,
    },
    /// A constant's byte length does not match its declared width.
    ConstantBytes {
        /// The node index.
        at: u32,
        /// Required byte length (`⌈width/8⌉`).
        expected: usize,
        /// Byte length found.
        found: usize,
    },
    /// An [`NodeOp::Input`] references a slot outside the input declaration.
    InputSlotOutOfRange {
        /// The node index.
        at: u32,
        /// The out-of-range slot.
        slot: u16,
    },
    /// An input node's declared width disagrees with the declared input lane.
    InputWidthMismatch {
        /// The node index.
        at: u32,
        /// The node's declared width.
        declared: u16,
        /// The input lane's width.
        input: u16,
    },
    /// A lookup's key operand width disagrees with the table's key width.
    LookupKeyWidth {
        /// The node index.
        at: u32,
        /// The table's declared key width.
        expected: u16,
        /// The key operand's width.
        found: u16,
    },
    /// A lookup table is over-large or has a mis-sized entry.
    LookupBound {
        /// The node index.
        at: u32,
        /// What the bound rule requires.
        reason: &'static str,
    },
    /// A declared output references a node id outside the array.
    OutputRefOutOfRange {
        /// Which output (`"admit"`, `"refusal_code"`, `"membrane"`).
        which: &'static str,
        /// The out-of-range id.
        id: u32,
    },
    /// A predicate output references a non-1-bit lane.
    OutputNotPredicate {
        /// Which output.
        which: &'static str,
        /// The referenced lane's width.
        width: u16,
    },
    /// The refusal-code lane is wider than a `u64`.
    RefusalCodeTooWide {
        /// The lane width.
        width: u16,
    },
    /// The program exceeds the structural limits.
    Limit(LimitViolation),
    /// A re-derived certificate did not match the claimed one.
    CertificateMismatch,
    /// The bytes did not decode to a program.
    MalformedEncoding,
    /// The bytes decoded but are not the program's canonical encoding.
    NonCanonicalEncoding,
    /// Canonical re-encoding failed.
    Encoding,
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SchemaVersion { found } => {
                write!(f, "schema version {found} is not the frozen one")
            }
            Self::TooLarge => f.write_str("node count is not u32-indexable"),
            Self::InputWidthOutOfRange { slot, width } => {
                write!(f, "input {slot} width {width} out of range")
            }
            Self::NodeWidthOutOfRange { at, width } => {
                write!(f, "node {at} width {width} out of range")
            }
            Self::Arity {
                at,
                expected,
                found,
            } => {
                write!(f, "node {at} arity {found} != required {expected}")
            }
            Self::OperandOutOfRange { at, operand } | Self::NonCanonicalEdge { at, operand } => {
                write!(
                    f,
                    "node {at} operand {operand} out of range or non-canonical"
                )
            }
            Self::OperandTyping { at, reason }
            | Self::OutputWidth { at, reason }
            | Self::LookupBound { at, reason } => write!(f, "node {at}: {reason}"),
            Self::ConstantBytes {
                at,
                expected,
                found,
            } => {
                write!(f, "node {at} constant has {found} bytes, needs {expected}")
            }
            Self::InputSlotOutOfRange { at, slot } => {
                write!(f, "node {at} reads out-of-range input slot {slot}")
            }
            Self::InputWidthMismatch {
                at,
                declared,
                input,
            } => {
                write!(
                    f,
                    "node {at} declares width {declared}, input lane is {input}"
                )
            }
            Self::LookupKeyWidth {
                at,
                expected,
                found,
            } => {
                write!(f, "node {at} lookup key width {found} != table {expected}")
            }
            Self::OutputRefOutOfRange { which, id } => {
                write!(f, "{which} output references out-of-range node {id}")
            }
            Self::OutputNotPredicate { which, width } => {
                write!(f, "{which} output is width {width}, not a predicate bit")
            }
            Self::RefusalCodeTooWide { width } => {
                write!(f, "refusal-code lane width {width} exceeds u64")
            }
            Self::Limit(violation) => write!(f, "structural limit: {violation:?}"),
            Self::CertificateMismatch => f.write_str("re-derived certificate mismatch"),
            Self::MalformedEncoding => f.write_str("bytes did not decode to a program"),
            Self::NonCanonicalEncoding => f.write_str("bytes are not the canonical encoding"),
            Self::Encoding => f.write_str("canonical re-encoding failed"),
        }
    }
}

impl std::error::Error for ValidationError {}

/// Whether a width is in the frozen `1 ..= MAX_WIDTH` range.
fn width_in_range(width: Width) -> bool {
    (1..=MAX_WIDTH).contains(&width.get())
}

fn check_constant(at: u32, bytes: &[u8], declared: usize) -> Result<(), ValidationError> {
    let need = declared.div_ceil(8);
    if bytes.len() == need {
        Ok(())
    } else {
        Err(ValidationError::ConstantBytes {
            at,
            expected: need,
            found: bytes.len(),
        })
    }
}

fn check_input(
    at: u32,
    slot: InputSlot,
    declared: usize,
    inputs: &[InputDecl],
) -> Result<(), ValidationError> {
    let decl = inputs
        .get(usize::from(slot.0))
        .ok_or(ValidationError::InputSlotOutOfRange { at, slot: slot.0 })?;
    if usize::from(decl.width.get()) == declared {
        Ok(())
    } else {
        Err(ValidationError::InputWidthMismatch {
            at,
            declared: declared_u16(declared),
            input: decl.width.get(),
        })
    }
}

fn check_predicate_binary(at: u32, ow: &[usize], declared: usize) -> Result<(), ValidationError> {
    if ow[0] != ow[1] {
        return Err(ValidationError::OperandTyping {
            at,
            reason: "binary operands must share a width",
        });
    }
    require_predicate_output(at, declared)
}

fn check_intersection(at: u32, ow: &[usize], declared: usize) -> Result<(), ValidationError> {
    if ow[0] != ow[1] {
        return Err(ValidationError::OperandTyping {
            at,
            reason: "intersection operands must share a width",
        });
    }
    if declared == ow[0] {
        Ok(())
    } else {
        Err(ValidationError::OutputWidth {
            at,
            reason: "intersection output width must equal the operand width",
        })
    }
}

fn check_bool_binary(at: u32, ow: &[usize], declared: usize) -> Result<(), ValidationError> {
    if ow[0] != 1 || ow[1] != 1 {
        return Err(ValidationError::OperandTyping {
            at,
            reason: "boolean operands must be 1 bit",
        });
    }
    require_predicate_output(at, declared)
}

fn check_bool_unary(at: u32, ow: &[usize], declared: usize) -> Result<(), ValidationError> {
    if ow[0] != 1 {
        return Err(ValidationError::OperandTyping {
            at,
            reason: "NOT operand must be 1 bit",
        });
    }
    require_predicate_output(at, declared)
}

fn check_select(at: u32, ow: &[usize], declared: usize) -> Result<(), ValidationError> {
    if ow[0] != 1 {
        return Err(ValidationError::OperandTyping {
            at,
            reason: "select condition must be 1 bit",
        });
    }
    if ow[1] != ow[2] {
        return Err(ValidationError::OperandTyping {
            at,
            reason: "select arms must share a width",
        });
    }
    if declared == ow[1] {
        Ok(())
    } else {
        Err(ValidationError::OutputWidth {
            at,
            reason: "select output width must equal the arm width",
        })
    }
}

fn check_lookup(
    at: u32,
    table: &LookupTable,
    ow: &[usize],
    declared: usize,
) -> Result<(), ValidationError> {
    if !width_in_range(table.key_width) {
        return Err(ValidationError::LookupBound {
            at,
            reason: "key width out of range",
        });
    }
    if ow[0] != usize::from(table.key_width.get()) {
        return Err(ValidationError::LookupKeyWidth {
            at,
            expected: table.key_width.get(),
            found: declared_u16(ow[0]),
        });
    }
    if table.entries.len() > MAX_LOOKUP_ENTRIES {
        return Err(ValidationError::LookupBound {
            at,
            reason: "entry count exceeds MAX_LOOKUP_ENTRIES",
        });
    }
    let need = declared.div_ceil(8);
    if table.entries.iter().any(|entry| entry.len() != need) {
        return Err(ValidationError::LookupBound {
            at,
            reason: "an entry's byte length does not match the output width",
        });
    }
    Ok(())
}

/// A predicate op must declare a 1-bit output.
fn require_predicate_output(at: u32, declared: usize) -> Result<(), ValidationError> {
    if declared == 1 {
        Ok(())
    } else {
        Err(ValidationError::OutputWidth {
            at,
            reason: "predicate output must be 1 bit",
        })
    }
}

/// Saturating `usize -> u16` for error display fields.
fn declared_u16(value: usize) -> u16 {
    u16::try_from(value).unwrap_or(u16::MAX)
}

/// Dispatch the per-op typing check.
fn validate_op(
    at: u32,
    op: &NodeOp,
    ow: &[usize],
    declared: usize,
    inputs: &[InputDecl],
) -> Result<(), ValidationError> {
    match op {
        NodeOp::Constant { bytes } => check_constant(at, bytes, declared),
        NodeOp::Input { slot } => check_input(at, *slot, declared, inputs),
        NodeOp::Eq | NodeOp::Compare { .. } | NodeOp::BitsetSubset => {
            check_predicate_binary(at, ow, declared)
        }
        NodeOp::BitsetIntersection => check_intersection(at, ow, declared),
        NodeOp::And | NodeOp::Or => check_bool_binary(at, ow, declared),
        NodeOp::Not => check_bool_unary(at, ow, declared),
        NodeOp::Select => check_select(at, ow, declared),
        NodeOp::BoundedLookup { table } => check_lookup(at, table, ow, declared),
    }
}

/// Validate one node: width range, arity, operand canonicality, and op typing.
fn validate_node(
    at: u32,
    node: &Node,
    node_count: u32,
    widths: &[Width],
    inputs: &[InputDecl],
) -> Result<(), ValidationError> {
    if !width_in_range(node.width) {
        return Err(ValidationError::NodeWidthOutOfRange {
            at,
            width: node.width.get(),
        });
    }
    let arity = node.op.operand_count();
    if node.operands.len() != arity {
        return Err(ValidationError::Arity {
            at,
            expected: arity,
            found: node.operands.len(),
        });
    }
    let mut ow: Vec<usize> = Vec::with_capacity(arity);
    for operand in &node.operands {
        if operand.0 >= node_count {
            return Err(ValidationError::OperandOutOfRange {
                at,
                operand: operand.0,
            });
        }
        if operand.0 >= at {
            return Err(ValidationError::NonCanonicalEdge {
                at,
                operand: operand.0,
            });
        }
        let idx = usize::try_from(operand.0).map_err(|_| ValidationError::OperandOutOfRange {
            at,
            operand: operand.0,
        })?;
        let width = widths
            .get(idx)
            .copied()
            .ok_or(ValidationError::OperandOutOfRange {
                at,
                operand: operand.0,
            })?;
        ow.push(usize::from(width.get()));
    }
    validate_op(at, &node.op, &ow, usize::from(node.width.get()), inputs)
}

fn validate_inputs(inputs: &[InputDecl]) -> Result<(), ValidationError> {
    for (slot, decl) in inputs.iter().enumerate() {
        if !width_in_range(decl.width) {
            return Err(ValidationError::InputWidthOutOfRange {
                slot,
                width: decl.width.get(),
            });
        }
    }
    Ok(())
}

/// Validate every node in canonical order, returning the per-node output widths.
fn validate_nodes(program: &AdmissionProgram) -> Result<Vec<Width>, ValidationError> {
    let nodes = program.nodes();
    let node_count = u32::try_from(nodes.len()).map_err(|_| ValidationError::TooLarge)?;
    let inputs = program.inputs();
    let mut widths: Vec<Width> = Vec::with_capacity(nodes.len());
    for (i, node) in nodes.iter().enumerate() {
        let at = u32::try_from(i).map_err(|_| ValidationError::TooLarge)?;
        validate_node(at, node, node_count, &widths, inputs)?;
        widths.push(node.width);
    }
    Ok(widths)
}

/// Resolve an output reference to its lane width.
fn output_width(widths: &[Width], id: NodeId, which: &'static str) -> Result<u16, ValidationError> {
    usize::try_from(id.0)
        .ok()
        .and_then(|i| widths.get(i))
        .map(|w| w.get())
        .ok_or(ValidationError::OutputRefOutOfRange { which, id: id.0 })
}

fn validate_outputs(outputs: &Outputs, widths: &[Width]) -> Result<(), ValidationError> {
    let admit_width = output_width(widths, outputs.admit, "admit")?;
    if admit_width != 1 {
        return Err(ValidationError::OutputNotPredicate {
            which: "admit",
            width: admit_width,
        });
    }
    let refusal_width = output_width(widths, outputs.refusal_code, "refusal_code")?;
    if refusal_width > 64 {
        return Err(ValidationError::RefusalCodeTooWide {
            width: refusal_width,
        });
    }
    for membrane in &outputs.membranes {
        let width = output_width(widths, *membrane, "membrane")?;
        if width != 1 {
            return Err(ValidationError::OutputNotPredicate {
                which: "membrane",
                width,
            });
        }
    }
    Ok(())
}

/// Validate a program against the structural limits, FAIL-CLOSED on the first
/// fault, returning the independently re-derived [`ProgramCertificate`] on success.
///
/// # Errors
/// The first [`ValidationError`] found.
pub fn validate(
    program: &AdmissionProgram,
    limits: &ProgramLimits,
) -> Result<ProgramCertificate, ValidationError> {
    if program.schema_version() != ADMISSION_PROGRAM_SCHEMA_VERSION {
        return Err(ValidationError::SchemaVersion {
            found: program.schema_version(),
        });
    }
    validate_inputs(program.inputs())?;
    let widths = validate_nodes(program)?;
    validate_outputs(program.outputs(), &widths)?;
    limits.check(program).map_err(ValidationError::Limit)?;
    program.certify().map_err(|_| ValidationError::Encoding)
}

/// Verify a claimed certificate by independently re-deriving it.
///
/// # Errors
/// [`ValidationError::CertificateMismatch`] if the re-derived certificate differs,
/// or any [`ValidationError`] from validating the program.
pub fn verify_certificate(
    program: &AdmissionProgram,
    claimed: &ProgramCertificate,
    limits: &ProgramLimits,
) -> Result<(), ValidationError> {
    let derived = validate(program, limits)?;
    if &derived == claimed {
        Ok(())
    } else {
        Err(ValidationError::CertificateMismatch)
    }
}

/// The untrusted-bytes wall: decode a program, validate it, and require the bytes
/// to be its canonical encoding (so a valid-but-non-canonical encoding is rejected).
///
/// # Errors
/// [`ValidationError::MalformedEncoding`] / [`ValidationError::NonCanonicalEncoding`]
/// for byte faults, or any structural [`ValidationError`].
pub fn decode_validated(
    bytes: &[u8],
    limits: &ProgramLimits,
) -> Result<AdmissionProgram, ValidationError> {
    let program: AdmissionProgram =
        batpak::canonical::from_bytes(bytes).map_err(|_| ValidationError::MalformedEncoding)?;
    validate(&program, limits)?;
    let reencoded = program
        .canonical_bytes()
        .map_err(|_| ValidationError::Encoding)?;
    if reencoded.as_slice() == bytes {
        Ok(program)
    } else {
        Err(ValidationError::NonCanonicalEncoding)
    }
}

#[cfg(test)]
mod validate_tests {
    use super::super::limits::{ProgramLimits, FROZEN_LIMITS};
    use super::super::program::{
        AdmissionProgram, CompareRel, InputDecl, InputSlot, Node, NodeId, NodeOp, Outputs, Width,
        ADMISSION_PROGRAM_SCHEMA_VERSION,
    };
    use super::{decode_validated, validate, verify_certificate, ValidationError};

    fn w(bits: u16) -> Width {
        Width::new(bits).expect("valid width")
    }

    /// A valid program: `admit = (in0 ≤ in1)` over 8-bit lanes.
    fn valid() -> AdmissionProgram {
        AdmissionProgram::new(
            vec![InputDecl { width: w(8) }, InputDecl { width: w(8) }],
            vec![
                Node {
                    op: NodeOp::Input { slot: InputSlot(0) },
                    operands: vec![],
                    width: w(8),
                },
                Node {
                    op: NodeOp::Input { slot: InputSlot(1) },
                    operands: vec![],
                    width: w(8),
                },
                Node {
                    op: NodeOp::Compare {
                        rel: CompareRel::Ule,
                    },
                    operands: vec![NodeId(0), NodeId(1)],
                    width: Width::one(),
                },
            ],
            Outputs {
                admit: NodeId(2),
                refusal_code: NodeId(2),
                membranes: vec![NodeId(2)],
            },
        )
        .expect("well-formed")
    }

    #[test]
    fn valid_program_validates_and_recovers_its_certificate() {
        let program = valid();
        let cert = validate(&program, &FROZEN_LIMITS).expect("valid");
        assert_eq!(cert, program.certify().expect("certify"));
        assert!(verify_certificate(&program, &cert, &FROZEN_LIMITS).is_ok());
    }

    #[test]
    fn tampered_certificate_is_rejected() {
        let program = valid();
        let mut cert = program.certify().expect("certify");
        cert.bit_depth += 1;
        assert_eq!(
            verify_certificate(&program, &cert, &FROZEN_LIMITS),
            Err(ValidationError::CertificateMismatch)
        );
    }

    #[test]
    fn wrong_schema_version_is_rejected() {
        let v = valid();
        let bad = AdmissionProgram::from_parts_unchecked(
            ADMISSION_PROGRAM_SCHEMA_VERSION + 1,
            v.inputs().to_vec(),
            v.nodes().to_vec(),
            v.outputs().clone(),
        );
        assert_eq!(
            validate(&bad, &FROZEN_LIMITS),
            Err(ValidationError::SchemaVersion {
                found: ADMISSION_PROGRAM_SCHEMA_VERSION + 1
            })
        );
    }

    #[test]
    fn out_of_range_width_is_rejected() {
        // A deserialized-style program with a width of 0 (impossible via Width::new).
        let bad = AdmissionProgram::from_parts_unchecked(
            ADMISSION_PROGRAM_SCHEMA_VERSION,
            vec![],
            vec![Node {
                op: NodeOp::Input { slot: InputSlot(0) },
                operands: vec![],
                width: Width::from_raw(0),
            }],
            Outputs {
                admit: NodeId(0),
                refusal_code: NodeId(0),
                membranes: vec![],
            },
        );
        assert_eq!(
            validate(&bad, &FROZEN_LIMITS),
            Err(ValidationError::NodeWidthOutOfRange { at: 0, width: 0 })
        );
    }

    #[test]
    fn arity_mismatch_is_rejected() {
        let bad = AdmissionProgram::from_parts_unchecked(
            ADMISSION_PROGRAM_SCHEMA_VERSION,
            vec![],
            vec![Node {
                op: NodeOp::And,
                operands: vec![],
                width: Width::one(),
            }],
            Outputs {
                admit: NodeId(0),
                refusal_code: NodeId(0),
                membranes: vec![],
            },
        );
        assert_eq!(
            validate(&bad, &FROZEN_LIMITS),
            Err(ValidationError::Arity {
                at: 0,
                expected: 2,
                found: 0
            })
        );
    }

    #[test]
    fn non_canonical_edge_is_rejected() {
        // node 0 references node 1 (a forward edge) — serde could produce this.
        let bad = AdmissionProgram::from_parts_unchecked(
            ADMISSION_PROGRAM_SCHEMA_VERSION,
            vec![InputDecl {
                width: Width::one(),
            }],
            vec![
                Node {
                    op: NodeOp::Not,
                    operands: vec![NodeId(1)],
                    width: Width::one(),
                },
                Node {
                    op: NodeOp::Input { slot: InputSlot(0) },
                    operands: vec![],
                    width: Width::one(),
                },
            ],
            Outputs {
                admit: NodeId(0),
                refusal_code: NodeId(0),
                membranes: vec![],
            },
        );
        assert_eq!(
            validate(&bad, &FROZEN_LIMITS),
            Err(ValidationError::NonCanonicalEdge { at: 0, operand: 1 })
        );
    }

    #[test]
    fn operand_width_typing_is_rejected() {
        // Eq over a 1-bit and an 8-bit operand: widths must match.
        let bad = AdmissionProgram::from_parts_unchecked(
            ADMISSION_PROGRAM_SCHEMA_VERSION,
            vec![
                InputDecl {
                    width: Width::one(),
                },
                InputDecl { width: w(8) },
            ],
            vec![
                Node {
                    op: NodeOp::Input { slot: InputSlot(0) },
                    operands: vec![],
                    width: Width::one(),
                },
                Node {
                    op: NodeOp::Input { slot: InputSlot(1) },
                    operands: vec![],
                    width: w(8),
                },
                Node {
                    op: NodeOp::Eq,
                    operands: vec![NodeId(0), NodeId(1)],
                    width: Width::one(),
                },
            ],
            Outputs {
                admit: NodeId(2),
                refusal_code: NodeId(2),
                membranes: vec![NodeId(2)],
            },
        );
        assert!(matches!(
            validate(&bad, &FROZEN_LIMITS),
            Err(ValidationError::OperandTyping { at: 2, .. })
        ));
    }

    #[test]
    fn predicate_output_used_as_wide_admit_is_rejected() {
        // admit references an 8-bit lane, not a predicate bit.
        let bad = AdmissionProgram::from_parts_unchecked(
            ADMISSION_PROGRAM_SCHEMA_VERSION,
            vec![InputDecl { width: w(8) }],
            vec![Node {
                op: NodeOp::Input { slot: InputSlot(0) },
                operands: vec![],
                width: w(8),
            }],
            Outputs {
                admit: NodeId(0),
                refusal_code: NodeId(0),
                membranes: vec![],
            },
        );
        assert!(matches!(
            validate(&bad, &FROZEN_LIMITS),
            Err(ValidationError::OutputNotPredicate {
                which: "admit",
                width: 8
            })
        ));
    }

    #[test]
    fn structural_limit_is_enforced() {
        // A valid 8-bit program against limits that cap width at 4 bits.
        let program = valid();
        let strict = ProgramLimits {
            max_width: 4,
            ..FROZEN_LIMITS
        };
        assert!(matches!(
            validate(&program, &strict),
            Err(ValidationError::Limit(_))
        ));
    }

    #[test]
    fn canonical_bytes_round_trip_and_reject_garbage() {
        let program = valid();
        let bytes = program.canonical_bytes().expect("encode");
        assert_eq!(
            decode_validated(&bytes, &FROZEN_LIMITS).expect("decode"),
            program
        );
        assert_eq!(
            decode_validated(&[0xff, 0x00, 0x13], &FROZEN_LIMITS),
            Err(ValidationError::MalformedEncoding)
        );
    }
}
