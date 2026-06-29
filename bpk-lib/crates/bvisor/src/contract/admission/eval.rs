//! The admission program evaluator `E : (A, x) -> decision` — the NC¹ computation.
//!
//! Pure, total, and FAIL-CLOSED: any structurally impossible input (wrong operand
//! count is already excluded by [`super::program::AdmissionProgram::new`], but a
//! mistyped width, an out-of-range input slot, or a too-wide refusal lane can still
//! reach here) is a typed [`EvalError`], never a panic. Totality is the property
//! the equivalence and fuzz harnesses rely on: a hostile or random program yields
//! an error, not a crash.
//!
//! A value flows as a [`Lane`] — `width` bits, least-significant first. The forward
//! pass visits nodes in canonical order; every operand precedes its node, so each
//! node's operand lanes are already computed.

use super::program::{
    AdmissionProgram, CompareRel, InputSlot, LookupTable, Node, NodeId, NodeOp, Outputs, Width,
};
use std::cmp::Ordering;

/// A value lane: a sequence of bits, least-significant bit first. Its length is the
/// lane's bit width.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lane {
    bits: Vec<bool>,
}

impl Lane {
    /// A lane from explicit bits (least-significant first).
    #[must_use]
    pub fn from_bits(bits: Vec<bool>) -> Self {
        Self { bits }
    }

    /// An all-zero lane of `width` bits.
    #[must_use]
    pub fn zeros(width: Width) -> Self {
        Self {
            bits: vec![false; usize::from(width.get())],
        }
    }

    /// A single-bit lane.
    #[must_use]
    pub fn bit(value: bool) -> Self {
        Self { bits: vec![value] }
    }

    /// Decode `width` bits from little-endian `bytes`, zero-extending if `bytes` is
    /// short (the validator rejects a length mismatch; this stays total).
    #[must_use]
    pub fn from_le_bytes(bytes: &[u8], width: Width) -> Self {
        let mut bits = vec![false; usize::from(width.get())];
        for (i, slot) in bits.iter_mut().enumerate() {
            if let Some(byte) = bytes.get(i / 8) {
                *slot = (byte >> (i % 8)) & 1 == 1;
            }
        }
        Self { bits }
    }

    /// The lane's bit width.
    #[must_use]
    pub fn width_bits(&self) -> usize {
        self.bits.len()
    }

    /// The bits, least-significant first.
    #[must_use]
    pub fn bits(&self) -> &[bool] {
        &self.bits
    }

    /// Bit 0 (the predicate value of a 1-bit lane), or `false` if empty.
    #[must_use]
    fn low_bit(&self) -> bool {
        self.bits.first().copied().unwrap_or(false)
    }

    /// The lane's value as a `u64`, or `None` if any set bit is at index ≥ 64.
    #[must_use]
    fn to_u64(&self) -> Option<u64> {
        let mut value: u64 = 0;
        for (i, bit) in self.bits.iter().enumerate() {
            if *bit {
                if i >= 64 {
                    return None;
                }
                value |= 1u64 << i;
            }
        }
        Some(value)
    }

    /// Unsigned comparison of two equal-width lanes (caller ensures equal width):
    /// scan from the most-significant bit for the first difference.
    #[must_use]
    fn cmp_unsigned(&self, other: &Self) -> Ordering {
        for (a, b) in self.bits.iter().rev().zip(other.bits.iter().rev()) {
            match (a, b) {
                (true, false) => return Ordering::Greater,
                (false, true) => return Ordering::Less,
                _ => {}
            }
        }
        Ordering::Equal
    }
}

/// The decision `E` produces: the admission bit, the first-failed-membrane refusal
/// code (meaningful when `admit` is `false`), and the per-membrane pass/fail bits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decision {
    /// Whether the boundary is admitted.
    pub admit: bool,
    /// The refusal-code lane's value (the first-failed membrane index).
    pub refusal_code: u64,
    /// Per-membrane pass/fail bits, in fixed membrane order.
    pub membranes: Vec<bool>,
}

/// Why a program could not be evaluated over a given input vector. Every variant is
/// a fail-closed typed refusal; `E` never panics.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum EvalError {
    /// The input vector's length does not match the program's input declaration.
    InputCountMismatch {
        /// Declared input count.
        expected: usize,
        /// Provided input count.
        found: usize,
    },
    /// A provided input lane's width does not match its declared width.
    InputWidthMismatch {
        /// The input slot.
        slot: usize,
        /// Declared width.
        expected: usize,
        /// Provided width.
        found: usize,
    },
    /// An [`NodeOp::Input`] node references a slot outside the input vector.
    InputSlotOutOfRange {
        /// The node index.
        at: u32,
        /// The out-of-range slot.
        slot: u16,
    },
    /// A [`NodeOp::Constant`]'s byte length does not match its declared width.
    ConstantWidthMismatch {
        /// The node index.
        at: u32,
        /// Expected byte length (`⌈width/8⌉`).
        expected: usize,
        /// Provided byte length.
        found: usize,
    },
    /// Two operands that must share a width do not.
    OperandWidthMismatch {
        /// The node index.
        at: u32,
        /// One operand's width.
        left: usize,
        /// The other operand's width.
        right: usize,
    },
    /// A node's declared output width disagrees with what its op produces.
    ResultWidthMismatch {
        /// The node index.
        at: u32,
        /// The width the op produces.
        expected: usize,
        /// The node's declared width.
        found: usize,
    },
    /// The refusal-code lane is wider than a `u64` can represent.
    RefusalCodeTooWide {
        /// The lane width.
        width: usize,
    },
    /// An output references a non-1-bit lane where a predicate bit is required.
    NonPredicateOutput {
        /// Which output (`"admit"` or `"membrane"`).
        which: &'static str,
        /// The lane's width.
        width: usize,
    },
    /// An internal reference did not resolve (a non-canonical program reached the
    /// evaluator; the validator excludes these structurally).
    Malformed {
        /// The node index where resolution failed.
        at: u32,
    },
}

impl std::fmt::Display for EvalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InputCountMismatch { expected, found } => {
                write!(f, "input count {found} != declared {expected}")
            }
            Self::InputWidthMismatch {
                slot,
                expected,
                found,
            } => write!(f, "input {slot} width {found} != declared {expected}"),
            Self::InputSlotOutOfRange { at, slot } => {
                write!(f, "node {at} reads out-of-range input slot {slot}")
            }
            Self::ConstantWidthMismatch {
                at,
                expected,
                found,
            } => write!(f, "node {at} constant has {found} bytes, needs {expected}"),
            Self::OperandWidthMismatch { at, left, right } => {
                write!(f, "node {at} operand widths {left} != {right}")
            }
            Self::ResultWidthMismatch {
                at,
                expected,
                found,
            } => write!(
                f,
                "node {at} declares width {found}, op produces {expected}"
            ),
            Self::RefusalCodeTooWide { width } => {
                write!(f, "refusal-code lane width {width} exceeds u64")
            }
            Self::NonPredicateOutput { which, width } => {
                write!(f, "{which} output is width {width}, not a predicate bit")
            }
            Self::Malformed { at } => write!(f, "node {at} has an unresolved reference"),
        }
    }
}

impl std::error::Error for EvalError {}

/// Resolve a [`NodeId`] to an already-computed lane in the forward pass.
fn resolve(values: &[Lane], id: NodeId, at: u32) -> Result<&Lane, EvalError> {
    usize::try_from(id.0)
        .ok()
        .and_then(|i| values.get(i))
        .ok_or(EvalError::Malformed { at })
}

/// Require a node's declared output width to equal what its op `produced`.
fn require_width(at: u32, declared: usize, produced: usize, lane: Lane) -> Result<Lane, EvalError> {
    if declared == produced {
        Ok(lane)
    } else {
        Err(EvalError::ResultWidthMismatch {
            at,
            expected: produced,
            found: declared,
        })
    }
}

/// Require two operands to share a width; return it.
fn require_pair(at: u32, a: &Lane, b: &Lane) -> Result<usize, EvalError> {
    if a.width_bits() == b.width_bits() {
        Ok(a.width_bits())
    } else {
        Err(EvalError::OperandWidthMismatch {
            at,
            left: a.width_bits(),
            right: b.width_bits(),
        })
    }
}

fn op_constant(at: u32, bytes: &[u8], width: Width, declared: usize) -> Result<Lane, EvalError> {
    let need = declared.div_ceil(8);
    if bytes.len() != need {
        return Err(EvalError::ConstantWidthMismatch {
            at,
            expected: need,
            found: bytes.len(),
        });
    }
    Ok(Lane::from_le_bytes(bytes, width))
}

fn op_input(at: u32, slot: InputSlot, declared: usize, inputs: &[Lane]) -> Result<Lane, EvalError> {
    let lane = inputs
        .get(usize::from(slot.0))
        .ok_or(EvalError::InputSlotOutOfRange { at, slot: slot.0 })?;
    require_width(at, declared, lane.width_bits(), lane.clone())
}

fn op_compare(
    at: u32,
    rel: CompareRel,
    a: &Lane,
    b: &Lane,
    declared: usize,
) -> Result<Lane, EvalError> {
    require_pair(at, a, b)?;
    let ord = a.cmp_unsigned(b);
    let value = match rel {
        CompareRel::Ule => ord != Ordering::Greater,
        CompareRel::Ult => ord == Ordering::Less,
    };
    require_width(at, declared, 1, Lane::bit(value))
}

fn op_subset(at: u32, a: &Lane, b: &Lane, declared: usize) -> Result<Lane, EvalError> {
    require_pair(at, a, b)?;
    let subset = a.bits().iter().zip(b.bits()).all(|(ai, bi)| !ai || *bi);
    require_width(at, declared, 1, Lane::bit(subset))
}

fn op_intersection(at: u32, a: &Lane, b: &Lane, declared: usize) -> Result<Lane, EvalError> {
    let width = require_pair(at, a, b)?;
    let bits = a
        .bits()
        .iter()
        .zip(b.bits())
        .map(|(ai, bi)| *ai && *bi)
        .collect();
    require_width(at, declared, width, Lane::from_bits(bits))
}

fn op_select(at: u32, cond: &Lane, a: &Lane, b: &Lane, declared: usize) -> Result<Lane, EvalError> {
    let width = require_pair(at, a, b)?;
    let chosen = if cond.low_bit() { a } else { b };
    require_width(at, declared, width, chosen.clone())
}

/// Evaluate one node given its already-computed operand lanes. A thin dispatcher;
/// each op's semantics live in a dedicated helper. Operand fan-in is guaranteed by
/// [`super::program::AdmissionProgram::new`], so positional indexing is in range.
fn eval_node(at: u32, node: &Node, operands: &[&Lane], inputs: &[Lane]) -> Result<Lane, EvalError> {
    let declared = usize::from(node.width.get());
    match &node.op {
        NodeOp::Constant { bytes } => op_constant(at, bytes, node.width, declared),
        NodeOp::Input { slot } => op_input(at, *slot, declared, inputs),
        NodeOp::Eq => op_eq(at, operands[0], operands[1], declared),
        NodeOp::Compare { rel } => op_compare(at, *rel, operands[0], operands[1], declared),
        NodeOp::BitsetSubset => op_subset(at, operands[0], operands[1], declared),
        NodeOp::BitsetIntersection => op_intersection(at, operands[0], operands[1], declared),
        NodeOp::And => require_width(
            at,
            declared,
            1,
            Lane::bit(operands[0].low_bit() && operands[1].low_bit()),
        ),
        NodeOp::Or => require_width(
            at,
            declared,
            1,
            Lane::bit(operands[0].low_bit() || operands[1].low_bit()),
        ),
        NodeOp::Not => require_width(at, declared, 1, Lane::bit(!operands[0].low_bit())),
        NodeOp::Select => op_select(at, operands[0], operands[1], operands[2], declared),
        NodeOp::BoundedLookup { table } => require_width(
            at,
            declared,
            declared,
            lookup(table, operands[0], node.width),
        ),
    }
}

fn op_eq(at: u32, a: &Lane, b: &Lane, declared: usize) -> Result<Lane, EvalError> {
    require_pair(at, a, b)?;
    require_width(at, declared, 1, Lane::bit(a.bits() == b.bits()))
}

/// A bounded lookup: the key lane's value selects an entry; an out-of-range or
/// too-wide key yields the all-zero lane (deterministic, fail-closed).
fn lookup(table: &LookupTable, key: &Lane, width: Width) -> Lane {
    match key.to_u64().and_then(|v| usize::try_from(v).ok()) {
        Some(index) => match table.entries.get(index) {
            Some(entry) => Lane::from_le_bytes(entry, width),
            None => Lane::zeros(width),
        },
        None => Lane::zeros(width),
    }
}

/// Read a single predicate bit from an output node, FAIL-CLOSED on a non-1-bit lane.
fn read_predicate(values: &[Lane], id: NodeId, which: &'static str) -> Result<bool, EvalError> {
    let lane = resolve(values, id, u32::MAX)?;
    if lane.width_bits() != 1 {
        return Err(EvalError::NonPredicateOutput {
            which,
            width: lane.width_bits(),
        });
    }
    Ok(lane.low_bit())
}

/// Evaluate `program` over the input vector `inputs` (indexed by input slot).
///
/// # Errors
/// An [`EvalError`] for any input/width/typing fault; the evaluation itself never
/// panics.
pub fn evaluate(program: &AdmissionProgram, inputs: &[Lane]) -> Result<Decision, EvalError> {
    let decls = program.inputs();
    if inputs.len() != decls.len() {
        return Err(EvalError::InputCountMismatch {
            expected: decls.len(),
            found: inputs.len(),
        });
    }
    for (slot, (decl, lane)) in decls.iter().zip(inputs).enumerate() {
        let expected = usize::from(decl.width.get());
        if lane.width_bits() != expected {
            return Err(EvalError::InputWidthMismatch {
                slot,
                expected,
                found: lane.width_bits(),
            });
        }
    }

    let mut values: Vec<Lane> = Vec::with_capacity(program.node_count());
    for (i, node) in program.nodes().iter().enumerate() {
        let at = u32::try_from(i).unwrap_or(u32::MAX);
        let lane = {
            let mut operand_lanes: Vec<&Lane> = Vec::with_capacity(node.operands.len());
            for operand in &node.operands {
                operand_lanes.push(resolve(&values, *operand, at)?);
            }
            eval_node(at, node, &operand_lanes, inputs)?
        };
        values.push(lane);
    }

    let Outputs {
        admit,
        refusal_code,
        membranes,
    } = program.outputs();

    let admit_bit = read_predicate(&values, *admit, "admit")?;
    let refusal_lane = resolve(&values, *refusal_code, u32::MAX)?;
    let refusal = refusal_lane.to_u64().ok_or(EvalError::RefusalCodeTooWide {
        width: refusal_lane.width_bits(),
    })?;
    let membrane_bits = membranes
        .iter()
        .map(|id| read_predicate(&values, *id, "membrane"))
        .collect::<Result<Vec<bool>, EvalError>>()?;

    Ok(Decision {
        admit: admit_bit,
        refusal_code: refusal,
        membranes: membrane_bits,
    })
}

#[cfg(test)]
mod eval_tests {
    use super::super::program::{
        AdmissionProgram, CompareRel, InputDecl, InputSlot, Node, NodeId, NodeOp, Outputs, Width,
    };
    use super::{evaluate, Decision, EvalError, Lane};

    fn w(bits: u16) -> Width {
        Width::new(bits).expect("valid width")
    }

    /// `admit = (in0 ≤ in1)` over 8-bit budget lanes; one membrane = the compare.
    fn budget_compare() -> AdmissionProgram {
        let nodes = vec![
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
        ];
        AdmissionProgram::new(
            vec![InputDecl { width: w(8) }, InputDecl { width: w(8) }],
            nodes,
            Outputs {
                admit: NodeId(2),
                refusal_code: NodeId(2),
                membranes: vec![NodeId(2)],
            },
        )
        .expect("well-formed")
    }

    fn byte_lane(value: u8) -> Lane {
        Lane::from_le_bytes(&[value], w(8))
    }

    #[test]
    fn budget_compare_admits_when_request_within_available() {
        let program = budget_compare();
        let admitted = evaluate(&program, &[byte_lane(10), byte_lane(20)]).expect("eval");
        assert_eq!(
            admitted,
            Decision {
                admit: true,
                refusal_code: 1,
                membranes: vec![true],
            }
        );
        // req == avail is admitted (Ule).
        assert!(
            evaluate(&program, &[byte_lane(20), byte_lane(20)])
                .expect("eval")
                .admit
        );
        // req > avail is refused.
        let refused = evaluate(&program, &[byte_lane(21), byte_lane(20)]).expect("eval");
        assert!(!refused.admit);
        assert_eq!(refused.membranes, vec![false]);
    }

    #[test]
    fn evaluation_is_deterministic() {
        let program = budget_compare();
        let a = evaluate(&program, &[byte_lane(7), byte_lane(9)]).expect("eval");
        let b = evaluate(&program, &[byte_lane(7), byte_lane(9)]).expect("eval");
        assert_eq!(a, b);
    }

    #[test]
    fn boolean_ops_and_select_compute_correctly() {
        // n0=in0(1) n1=in1(1) n2=Not(n0) n3=And(n1,n2) n4=Or(n0,n3)
        // n5=in2(8) n6=in3(8) n7=Select(n4, n5, n6)
        let nodes = vec![
            Node {
                op: NodeOp::Input { slot: InputSlot(0) },
                operands: vec![],
                width: Width::one(),
            },
            Node {
                op: NodeOp::Input { slot: InputSlot(1) },
                operands: vec![],
                width: Width::one(),
            },
            Node {
                op: NodeOp::Not,
                operands: vec![NodeId(0)],
                width: Width::one(),
            },
            Node {
                op: NodeOp::And,
                operands: vec![NodeId(1), NodeId(2)],
                width: Width::one(),
            },
            Node {
                op: NodeOp::Or,
                operands: vec![NodeId(0), NodeId(3)],
                width: Width::one(),
            },
            Node {
                op: NodeOp::Input { slot: InputSlot(2) },
                operands: vec![],
                width: w(8),
            },
            Node {
                op: NodeOp::Input { slot: InputSlot(3) },
                operands: vec![],
                width: w(8),
            },
            Node {
                op: NodeOp::Select,
                operands: vec![NodeId(4), NodeId(5), NodeId(6)],
                width: w(8),
            },
        ];
        let program = AdmissionProgram::new(
            vec![
                InputDecl {
                    width: Width::one(),
                },
                InputDecl {
                    width: Width::one(),
                },
                InputDecl { width: w(8) },
                InputDecl { width: w(8) },
            ],
            nodes,
            Outputs {
                admit: NodeId(4),
                refusal_code: NodeId(4),
                membranes: vec![NodeId(4)],
            },
        )
        .expect("well-formed");

        // in0=false, in1=true -> not=true, and=true, or(false,true)=true -> select picks a(=in2).
        let d = evaluate(
            &program,
            &[
                Lane::bit(false),
                Lane::bit(true),
                byte_lane(0xAA),
                byte_lane(0xBB),
            ],
        )
        .expect("eval");
        assert!(d.admit);
        // in0=true -> or=true regardless; or(true, and(true,not(true)=false)=false)=true.
        assert!(
            evaluate(
                &program,
                &[
                    Lane::bit(true),
                    Lane::bit(false),
                    byte_lane(1),
                    byte_lane(2)
                ],
            )
            .expect("eval")
            .admit
        );
    }

    #[test]
    fn bitset_subset_and_intersection() {
        // n0=in0(4) n1=in1(4) n2=Subset(n0,n1) ; also n3=Intersection(n0,n1)
        let nodes = vec![
            Node {
                op: NodeOp::Input { slot: InputSlot(0) },
                operands: vec![],
                width: w(4),
            },
            Node {
                op: NodeOp::Input { slot: InputSlot(1) },
                operands: vec![],
                width: w(4),
            },
            Node {
                op: NodeOp::BitsetSubset,
                operands: vec![NodeId(0), NodeId(1)],
                width: Width::one(),
            },
        ];
        let program = AdmissionProgram::new(
            vec![InputDecl { width: w(4) }, InputDecl { width: w(4) }],
            nodes,
            Outputs {
                admit: NodeId(2),
                refusal_code: NodeId(2),
                membranes: vec![NodeId(2)],
            },
        )
        .expect("well-formed");
        // 0b0101 ⊆ 0b0111 ? yes.
        let a = Lane::from_bits(vec![true, false, true, false]);
        let b = Lane::from_bits(vec![true, true, true, false]);
        assert!(evaluate(&program, &[a, b]).expect("eval").admit);
        // 0b0101 ⊆ 0b0001 ? no (bit 2 set in a, clear in b).
        let a2 = Lane::from_bits(vec![true, false, true, false]);
        let b2 = Lane::from_bits(vec![true, false, false, false]);
        assert!(!evaluate(&program, &[a2, b2]).expect("eval").admit);
    }

    #[test]
    fn fails_closed_on_input_count_mismatch() {
        let program = budget_compare();
        let err = evaluate(&program, &[byte_lane(1)]).expect_err("count");
        assert_eq!(
            err,
            EvalError::InputCountMismatch {
                expected: 2,
                found: 1
            }
        );
    }

    #[test]
    fn fails_closed_on_input_width_mismatch() {
        let program = budget_compare();
        let err = evaluate(&program, &[Lane::bit(true), byte_lane(2)]).expect_err("width");
        assert_eq!(
            err,
            EvalError::InputWidthMismatch {
                slot: 0,
                expected: 8,
                found: 1
            }
        );
    }

    #[test]
    fn fails_closed_when_input_slot_is_out_of_range() {
        // A program declaring ONE input but reading slot 5.
        let nodes = vec![Node {
            op: NodeOp::Input { slot: InputSlot(5) },
            operands: vec![],
            width: Width::one(),
        }];
        let program = AdmissionProgram::new(
            vec![InputDecl {
                width: Width::one(),
            }],
            nodes,
            Outputs {
                admit: NodeId(0),
                refusal_code: NodeId(0),
                membranes: vec![],
            },
        )
        .expect("structurally well-formed");
        let err = evaluate(&program, &[Lane::bit(true)]).expect_err("slot");
        assert_eq!(err, EvalError::InputSlotOutOfRange { at: 0, slot: 5 });
    }
}
