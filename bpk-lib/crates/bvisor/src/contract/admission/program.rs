//! The frozen `AdmissionProgram` IR — a bounded, canonical decision circuit.
//!
//! This module freezes the *representation* (build-order step 1): the closed node
//! vocabulary, the canonical topological encoding, the structural limits, the
//! bit-level depth recurrence, and the proof certificate. It deliberately does NOT
//! contain the compiler `C` (`(Spec, Profile) -> AdmissionProgram`, step 2), the
//! independent validator (step 3), or the evaluator `E` (step 4) — those consume
//! the artifacts frozen here.
//!
//! ## The theorem object (plan §1–§3)
//!
//! An [`AdmissionProgram`] is a directed acyclic circuit over a **fixed
//! vocabulary** of [`NodeOp`]s. It is the `A` in `C : (S,P) -> A`, `E : (A,x) ->
//! decision`. The NC¹ claim is made of `A`/`E` (an `O(log W)`-depth Boolean
//! circuit), never of the compiler that emits it.
//!
//! ### Canonical form
//!
//! Nodes are stored in a single canonical order in which **every operand index is
//! strictly less than the referencing node's index**. This makes acyclicity
//! structural, makes the depth recurrence a single forward pass, and makes the
//! canonical byte encoding (and therefore `H_A`) deterministic. [`AdmissionProgram::new`]
//! rejects any program that violates the invariant.
//!
//! ### Bit-level depth (the honest NC¹ accounting)
//!
//! Depth is counted at the **bit** level, not the word level — a comparator or a
//! bitset reduction over `W` bits contributes `O(log W)`, not `1`. The per-op cost
//! model is [`NodeOp::bit_cost`]; the recurrence is [`AdmissionProgram::bit_levels`].
//! The claim is "NC¹ *as Boolean circuits*," not "NC¹ relative to powerful
//! unit-cost primitives."

use crate::contract::ids::AdmissionProgramHash;
use serde::{Deserialize, Serialize};

/// Wire schema version of the frozen IR. The vocabulary, encoding, limits, depth
/// model, and certificate shape are FROZEN at this version; any change to them is
/// a schema bump, not an in-place edit.
pub const ADMISSION_PROGRAM_SCHEMA_VERSION: u16 = 1;

/// `⌈log₂ n⌉`, saturating at 0 for `n ≤ 1`. Pure integer math, no casts.
#[must_use]
pub(crate) fn ceil_log2(n: u32) -> u32 {
    match n {
        0 | 1 => 0,
        _ => u32::BITS - (n - 1).leading_zeros(),
    }
}

/// Convert a validated [`NodeId`] to a slice index. The constructor proves every
/// id is in range and fits `usize` on supported (32/64-bit) targets.
#[must_use]
fn index_of(id: NodeId) -> usize {
    usize::try_from(id.0).expect("a NodeId fits in usize on supported targets")
}

/// The bit width of a value lane. A node's declared *output* width; also the width
/// of an input lane. Always `1 ..= MAX_WIDTH`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Width(u16);

/// The widest single lane the frozen limits admit. Sized to cover a 64-bit budget
/// word and a 64-element capability/evidence bitset with headroom; the 7-dimension
/// budget vector is seven separate lanes, never one wide lane.
pub const MAX_WIDTH: u16 = 256;

impl Width {
    /// Construct a width, FAIL-CLOSED outside `1 ..= MAX_WIDTH`.
    #[must_use]
    pub fn new(bits: u16) -> Option<Self> {
        if (1..=MAX_WIDTH).contains(&bits) {
            Some(Self(bits))
        } else {
            None
        }
    }

    /// A single bit — the width of every predicate (membrane / admit) lane.
    #[must_use]
    pub const fn one() -> Self {
        Self(1)
    }

    /// The width in bits.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }

    /// Test-only: build a width WITHOUT the `1 ..= MAX_WIDTH` range check, so the
    /// independent validator can be exercised against out-of-range deserialized
    /// widths (the parse-don't-validate wall). Never reachable in production.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn from_raw(bits: u16) -> Self {
        Self(bits)
    }
}

/// Index of a declared input lane, referenced by [`NodeOp::Input`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct InputSlot(pub u16);

/// A fixed-width unsigned comparison relation carried by [`NodeOp::Compare`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum CompareRel {
    /// Unsigned `a ≤ b` (the budget-admission relation `req ≤ avail`).
    Ule,
    /// Unsigned `a < b`.
    Ult,
}

/// A frozen lookup table for [`NodeOp::BoundedLookup`]: the index operand selects
/// one entry. Bounded — `entries.len()` and each entry's length are structurally
/// limited.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LookupTable {
    /// Width of the index operand (in bits).
    pub key_width: Width,
    /// Output entries, indexed by the key value; each is a little-endian lane of
    /// the node's output width.
    pub entries: Vec<Vec<u8>>,
}

/// The FROZEN, CLOSED node vocabulary. This is the entire instruction set an
/// [`AdmissionProgram`] may use; the validator rejects anything else. Adding an op
/// is a schema bump (see [`ADMISSION_PROGRAM_SCHEMA_VERSION`]) — deliberately NOT
/// `#[non_exhaustive]`, because "frozen" and "open for extension" contradict.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeOp {
    /// A frozen constant lane (little-endian bytes of the node's output width).
    Constant {
        /// Little-endian value bytes; length is `⌈width/8⌉`.
        bytes: Vec<u8>,
    },
    /// Reads declared input lane `slot`.
    Input {
        /// The input lane to read.
        slot: InputSlot,
    },
    /// Fixed-width equality of two equal-width operands → 1 bit.
    Eq,
    /// Fixed-width unsigned comparison of two equal-width operands → 1 bit.
    Compare {
        /// The relation evaluated.
        rel: CompareRel,
    },
    /// Bitset subset `a ⊆ b` over two equal-width operands → 1 bit.
    BitsetSubset,
    /// Bitwise intersection `a & b` over two equal-width operands → width `W`.
    BitsetIntersection,
    /// Boolean AND of two 1-bit operands → 1 bit.
    And,
    /// Boolean OR of two 1-bit operands → 1 bit.
    Or,
    /// Boolean NOT of one 1-bit operand → 1 bit.
    Not,
    /// `SELECT(cond, a, b)`: `cond` 1-bit, `a`/`b` width `W` → width `W`.
    Select,
    /// Bounded table lookup: a key operand selects one frozen entry → width `W`.
    BoundedLookup {
        /// The frozen table.
        table: LookupTable,
    },
}

impl NodeOp {
    /// Number of operand edges this op consumes — part of the frozen well-formedness
    /// rules (the validator enforces it; the constructor checks fan-in).
    #[must_use]
    pub fn operand_count(&self) -> usize {
        match self {
            Self::Constant { .. } | Self::Input { .. } => 0,
            Self::Not | Self::BoundedLookup { .. } => 1,
            Self::Eq
            | Self::Compare { .. }
            | Self::BitsetSubset
            | Self::BitsetIntersection
            | Self::And
            | Self::Or => 2,
            Self::Select => 3,
        }
    }

    /// Whether this op produces a single predicate bit (versus a width-`W` lane).
    /// The frozen output-width rule for predicate ops.
    #[must_use]
    pub fn produces_single_bit(&self) -> bool {
        match self {
            Self::Eq
            | Self::Compare { .. }
            | Self::BitsetSubset
            | Self::And
            | Self::Or
            | Self::Not => true,
            Self::Constant { .. }
            | Self::Input { .. }
            | Self::BitsetIntersection
            | Self::Select
            | Self::BoundedLookup { .. } => false,
        }
    }

    /// The frozen **bit-level** depth this op contributes, given the governing
    /// operand width `w` (the width of the lanes it reduces over). Each cost is the
    /// depth of the op's Boolean-circuit lowering:
    ///
    /// - `Constant`/`Input`: `0` (sources).
    /// - `And`/`Or`/`Not`/`Select`/`BitsetIntersection`: `1` (per-bit gates,
    ///   evaluated in parallel across the lane).
    /// - `Eq`/`Compare`/`BitsetSubset`: `⌈log₂ w⌉ + 1` (a per-bit layer feeding a
    ///   balanced reduction over `w` bits — the parallel-prefix comparator / the
    ///   subset AND-reduction).
    /// - `BoundedLookup`: `⌈log₂ entries⌉ + 1` (a balanced mux tree over the
    ///   entries).
    #[must_use]
    pub fn bit_cost(&self, governing_width: Width) -> u32 {
        match self {
            Self::Constant { .. } | Self::Input { .. } => 0,
            Self::And | Self::Or | Self::Not | Self::Select | Self::BitsetIntersection => 1,
            Self::Eq | Self::Compare { .. } | Self::BitsetSubset => {
                ceil_log2(u32::from(governing_width.get())) + 1
            }
            Self::BoundedLookup { table } => {
                let entries = u32::try_from(table.entries.len()).unwrap_or(u32::MAX);
                ceil_log2(entries) + 1
            }
        }
    }
}

/// Index of a node within an [`AdmissionProgram`]'s canonical node array.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct NodeId(pub u32);

/// A declared input lane.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputDecl {
    /// The lane's bit width.
    pub width: Width,
}

/// One circuit node: an op, its operand edges, and its declared output width.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Node {
    /// The operation.
    pub op: NodeOp,
    /// Operand edges, each referencing a strictly-earlier node.
    pub operands: Vec<NodeId>,
    /// The node's output width.
    pub width: Width,
}

/// The program's declared outputs (plan §2): the admission bit, a refusal-code
/// lane (the first-failed membrane index), and one pass/fail bit per membrane.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outputs {
    /// The single admission bit (`1` = admit).
    pub admit: NodeId,
    /// The bounded refusal-code lane (first-failed membrane index; meaningful when
    /// `admit` is `0`).
    pub refusal_code: NodeId,
    /// Per-membrane pass/fail bits, in fixed membrane order.
    pub membranes: Vec<NodeId>,
}

/// Why a node array could not form a well-formed canonical [`AdmissionProgram`].
/// Constructor-level structural faults only; full vocabulary/width/limit validation
/// is the step-3 validator's job.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ProgramError {
    /// An operand (or output) references a node id outside the array.
    NodeIdOutOfRange {
        /// The referencing node's index (or `u32::MAX` for an output reference).
        at: u32,
        /// The out-of-range id.
        referenced: NodeId,
    },
    /// An operand references the node itself or a later node — a forward/self edge
    /// that would break the canonical topological invariant.
    NonCanonicalEdge {
        /// The referencing node's index.
        at: u32,
        /// The forward/self operand.
        referenced: NodeId,
    },
    /// A node's operand count does not match its op's frozen arity.
    ArityMismatch {
        /// The node's index.
        at: u32,
        /// The arity the op requires.
        expected: usize,
        /// The arity found.
        found: usize,
    },
    /// The node array is larger than `u32` can index.
    TooManyNodes {
        /// The offending node count.
        count: usize,
    },
}

impl std::fmt::Display for ProgramError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NodeIdOutOfRange { at, referenced } => {
                write!(f, "node {at} references out-of-range id {}", referenced.0)
            }
            Self::NonCanonicalEdge { at, referenced } => write!(
                f,
                "node {at} has a non-canonical (forward/self) operand {}",
                referenced.0
            ),
            Self::ArityMismatch {
                at,
                expected,
                found,
            } => write!(f, "node {at} has arity {found}, op requires {expected}"),
            Self::TooManyNodes { count } => {
                write!(f, "{count} nodes exceeds the u32-indexable maximum")
            }
        }
    }
}

impl std::error::Error for ProgramError {}

/// A bounded, canonical admission decision circuit. Constructing one proves the
/// canonical-topological invariant (operands reference strictly-earlier nodes,
/// arity matches the op); deeper acceptance — vocabulary closure, width/arity
/// typing, structural limits, canonical re-encoding — is the independent
/// validator's contract (step 3).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdmissionProgram {
    schema_version: u16,
    inputs: Vec<InputDecl>,
    nodes: Vec<Node>,
    outputs: Outputs,
}

impl AdmissionProgram {
    /// Build a program, enforcing the canonical-topological invariant FAIL-CLOSED.
    ///
    /// # Errors
    /// [`ProgramError`] if the node array is not `u32`-indexable, any operand or
    /// output references an out-of-range id, any operand is a forward/self edge, or
    /// any node's fan-in disagrees with its op's frozen arity.
    pub fn new(
        inputs: Vec<InputDecl>,
        nodes: Vec<Node>,
        outputs: Outputs,
    ) -> Result<Self, ProgramError> {
        let count = nodes.len();
        let node_count = u32::try_from(count).map_err(|_| ProgramError::TooManyNodes { count })?;

        for (i, node) in nodes.iter().enumerate() {
            let at = u32::try_from(i).map_err(|_| ProgramError::TooManyNodes { count })?;

            let expected = node.op.operand_count();
            if node.operands.len() != expected {
                return Err(ProgramError::ArityMismatch {
                    at,
                    expected,
                    found: node.operands.len(),
                });
            }

            for operand in &node.operands {
                if operand.0 >= node_count {
                    return Err(ProgramError::NodeIdOutOfRange {
                        at,
                        referenced: *operand,
                    });
                }
                if operand.0 >= at {
                    return Err(ProgramError::NonCanonicalEdge {
                        at,
                        referenced: *operand,
                    });
                }
            }
        }

        for output in outputs
            .membranes
            .iter()
            .chain([&outputs.admit, &outputs.refusal_code])
        {
            if output.0 >= node_count {
                return Err(ProgramError::NodeIdOutOfRange {
                    at: u32::MAX,
                    referenced: *output,
                });
            }
        }

        Ok(Self {
            schema_version: ADMISSION_PROGRAM_SCHEMA_VERSION,
            inputs,
            nodes,
            outputs,
        })
    }

    /// Test-only: assemble a program WITHOUT any well-formedness check, mirroring
    /// what serde `Deserialize` can produce from untrusted bytes. Used to prove the
    /// independent validator rejects malformed programs the typed constructor would
    /// never build. Never reachable in production.
    #[cfg(test)]
    pub(crate) fn from_parts_unchecked(
        schema_version: u16,
        inputs: Vec<InputDecl>,
        nodes: Vec<Node>,
        outputs: Outputs,
    ) -> Self {
        Self {
            schema_version,
            inputs,
            nodes,
            outputs,
        }
    }

    /// The schema version this program was built at.
    #[must_use]
    pub fn schema_version(&self) -> u16 {
        self.schema_version
    }

    /// The declared input lanes.
    #[must_use]
    pub fn inputs(&self) -> &[InputDecl] {
        &self.inputs
    }

    /// The canonical node array (topological order).
    #[must_use]
    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }

    /// The declared outputs.
    #[must_use]
    pub fn outputs(&self) -> &Outputs {
        &self.outputs
    }

    /// Number of nodes.
    #[must_use]
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// The widest lane across inputs and nodes (the `W` the limits are taken in).
    #[must_use]
    pub fn max_width(&self) -> Width {
        let from_inputs = self.inputs.iter().map(|d| d.width.get());
        let from_nodes = self.nodes.iter().map(|n| n.width.get());
        let widest = from_inputs.chain(from_nodes).max().unwrap_or(1);
        Width::new(widest).unwrap_or(Width::one())
    }

    /// The governing operand width for a node's bit-cost: the width of the lanes it
    /// reduces over (operand 0 for the binary reductions), or the node's own width
    /// for sources and per-bit ops.
    #[must_use]
    fn governing_width(&self, node: &Node) -> Width {
        match node.operands.first() {
            Some(first) => self.nodes[index_of(*first)].width,
            None => node.width,
        }
    }

    /// The frozen bit-level depth recurrence: a single forward pass over the
    /// canonical array. `level(v) = bit_cost(v) + max over operands of level(u)`,
    /// with sources at `0`. Safe and total because every operand precedes its node.
    #[must_use]
    pub fn bit_levels(&self) -> Vec<u32> {
        let mut levels: Vec<u32> = Vec::with_capacity(self.nodes.len());
        for node in &self.nodes {
            let cost = node.op.bit_cost(self.governing_width(node));
            let parent_max = node
                .operands
                .iter()
                .map(|operand| levels[index_of(*operand)])
                .max()
                .unwrap_or(0);
            levels.push(cost.saturating_add(parent_max));
        }
        levels
    }

    /// Total bit-level circuit depth `D(A)` — the max over [`Self::bit_levels`].
    #[must_use]
    pub fn bit_depth(&self) -> u32 {
        self.bit_levels().iter().copied().max().unwrap_or(0)
    }

    /// Canonical bytes (the frozen encoding `H_S`/`H_A` are taken over). Identical
    /// canonical programs produce identical bytes.
    ///
    /// # Errors
    /// [`rmp_serde::encode::Error`] if canonical encoding fails.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, rmp_serde::encode::Error> {
        batpak::canonical::to_bytes(self)
    }

    /// The program's content digest `H_A`.
    ///
    /// # Errors
    /// [`rmp_serde::encode::Error`] if canonical encoding fails.
    pub fn digest(&self) -> Result<AdmissionProgramHash, rmp_serde::encode::Error> {
        let bytes = self.canonical_bytes()?;
        Ok(AdmissionProgramHash(batpak::event::hash::compute_hash(
            &bytes,
        )))
    }

    /// Derive the proof certificate the step-3 validator independently re-checks.
    ///
    /// # Errors
    /// [`rmp_serde::encode::Error`] if canonical encoding fails (for the digest).
    pub fn certify(&self) -> Result<ProgramCertificate, rmp_serde::encode::Error> {
        let levels = self.bit_levels();
        let entries = self
            .nodes
            .iter()
            .zip(levels.iter().copied())
            .map(|(node, level)| CertNode {
                operands: node.operands.clone(),
                width: node.width,
                bit_level: level,
                single_bit: node.op.produces_single_bit(),
            })
            .collect();
        Ok(ProgramCertificate {
            schema_version: self.schema_version,
            node_count: self.nodes.len(),
            input_width: self.max_width(),
            bit_depth: levels.iter().copied().max().unwrap_or(0),
            nodes: entries,
            digest: self.digest()?,
        })
    }
}

/// One node's entry in a [`ProgramCertificate`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CertNode {
    /// Operand edges (each strictly earlier).
    pub operands: Vec<NodeId>,
    /// The node's output width.
    pub width: Width,
    /// The node's computed bit-level depth.
    pub bit_level: u32,
    /// Whether the node produces a single predicate bit.
    pub single_bit: bool,
}

/// The frozen proof certificate the compiler `C` emits and the independent
/// validator re-checks (plan §3): the canonical per-node levels/widths/edges, the
/// counts, the total bit-depth, and the digest. The validator recomputes every
/// field rather than trusting it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgramCertificate {
    /// The IR schema version certified.
    pub schema_version: u16,
    /// The node count.
    pub node_count: usize,
    /// The widest lane (`W`).
    pub input_width: Width,
    /// The total bit-level circuit depth `D(A)`.
    pub bit_depth: u32,
    /// Per-node certificate entries, in canonical order.
    pub nodes: Vec<CertNode>,
    /// The program digest `H_A`.
    pub digest: AdmissionProgramHash,
}

#[cfg(test)]
mod admission_tests {
    use super::{
        ceil_log2, AdmissionProgram, CompareRel, InputDecl, InputSlot, Node, NodeId, NodeOp,
        Outputs, ProgramError, Width, ADMISSION_PROGRAM_SCHEMA_VERSION, MAX_WIDTH,
    };

    fn w(bits: u16) -> Width {
        Width::new(bits).expect("valid width")
    }

    /// A tiny well-formed program: `admit = (in0 ≤ in1)` over 64-bit lanes.
    /// nodes: 0=Input(0,w64) 1=Input(1,w64) 2=Compare(Ule)[0,1] (1-bit).
    fn budget_compare_program() -> AdmissionProgram {
        let nodes = vec![
            Node {
                op: NodeOp::Input { slot: InputSlot(0) },
                operands: vec![],
                width: w(64),
            },
            Node {
                op: NodeOp::Input { slot: InputSlot(1) },
                operands: vec![],
                width: w(64),
            },
            Node {
                op: NodeOp::Compare {
                    rel: CompareRel::Ule,
                },
                operands: vec![NodeId(0), NodeId(1)],
                width: Width::one(),
            },
        ];
        let outputs = Outputs {
            admit: NodeId(2),
            refusal_code: NodeId(2),
            membranes: vec![NodeId(2)],
        };
        AdmissionProgram::new(
            vec![InputDecl { width: w(64) }, InputDecl { width: w(64) }],
            nodes,
            outputs,
        )
        .expect("well-formed")
    }

    #[test]
    fn ceil_log2_matches_hand_values() {
        assert_eq!(ceil_log2(0), 0);
        assert_eq!(ceil_log2(1), 0);
        assert_eq!(ceil_log2(2), 1);
        assert_eq!(ceil_log2(3), 2);
        assert_eq!(ceil_log2(4), 2);
        assert_eq!(ceil_log2(5), 3);
        assert_eq!(ceil_log2(64), 6);
        assert_eq!(ceil_log2(256), 8);
    }

    #[test]
    fn width_is_fail_closed_outside_range() {
        assert!(Width::new(0).is_none());
        assert!(Width::new(1).is_some());
        assert!(Width::new(MAX_WIDTH).is_some());
        assert!(Width::new(MAX_WIDTH + 1).is_none());
    }

    #[test]
    fn arity_is_frozen_per_op() {
        assert_eq!(NodeOp::Not.operand_count(), 1);
        assert_eq!(NodeOp::And.operand_count(), 2);
        assert_eq!(NodeOp::Eq.operand_count(), 2);
        assert_eq!(NodeOp::Select.operand_count(), 3);
        assert_eq!(NodeOp::Input { slot: InputSlot(0) }.operand_count(), 0);
    }

    #[test]
    fn bit_cost_is_bit_level_not_word_level() {
        // A 64-bit comparator is log-depth, not unit-depth and not 64.
        assert_eq!(
            NodeOp::Compare {
                rel: CompareRel::Ule
            }
            .bit_cost(w(64)),
            ceil_log2(64) + 1,
        );
        // Per-bit gates are unit depth regardless of width.
        assert_eq!(NodeOp::And.bit_cost(w(64)), 1);
        assert_eq!(NodeOp::BitsetIntersection.bit_cost(w(256)), 1);
        // Sources are free.
        assert_eq!(NodeOp::Input { slot: InputSlot(0) }.bit_cost(w(64)), 0);
    }

    #[test]
    fn construction_enforces_canonical_topological_order() {
        let program = budget_compare_program();
        assert_eq!(program.schema_version(), ADMISSION_PROGRAM_SCHEMA_VERSION);
        assert_eq!(program.node_count(), 3);
    }

    #[test]
    fn forward_edge_fails_closed() {
        // node 0 references node 1 — a forward edge.
        let nodes = vec![
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
        ];
        let outputs = Outputs {
            admit: NodeId(0),
            refusal_code: NodeId(0),
            membranes: vec![NodeId(0)],
        };
        let err = AdmissionProgram::new(
            vec![InputDecl {
                width: Width::one(),
            }],
            nodes,
            outputs,
        )
        .expect_err("forward edge");
        assert_eq!(
            err,
            ProgramError::NonCanonicalEdge {
                at: 0,
                referenced: NodeId(1),
            }
        );
    }

    #[test]
    fn arity_mismatch_fails_closed() {
        let nodes = vec![Node {
            op: NodeOp::And, // needs 2 operands
            operands: vec![],
            width: Width::one(),
        }];
        let outputs = Outputs {
            admit: NodeId(0),
            refusal_code: NodeId(0),
            membranes: vec![],
        };
        let err = AdmissionProgram::new(vec![], nodes, outputs).expect_err("arity");
        assert_eq!(
            err,
            ProgramError::ArityMismatch {
                at: 0,
                expected: 2,
                found: 0,
            }
        );
    }

    #[test]
    fn out_of_range_output_fails_closed() {
        let nodes = vec![Node {
            op: NodeOp::Input { slot: InputSlot(0) },
            operands: vec![],
            width: Width::one(),
        }];
        let outputs = Outputs {
            admit: NodeId(7), // no such node
            refusal_code: NodeId(0),
            membranes: vec![],
        };
        let err = AdmissionProgram::new(
            vec![InputDecl {
                width: Width::one(),
            }],
            nodes,
            outputs,
        )
        .expect_err("oob output");
        assert_eq!(
            err,
            ProgramError::NodeIdOutOfRange {
                at: u32::MAX,
                referenced: NodeId(7),
            }
        );
    }

    #[test]
    fn bit_levels_accumulate_along_the_longest_path() {
        let program = budget_compare_program();
        let levels = program.bit_levels();
        // inputs are sources (0); the comparator adds ⌈log₂64⌉+1 = 7.
        assert_eq!(levels, vec![0, 0, ceil_log2(64) + 1]);
        assert_eq!(program.bit_depth(), 7);
    }

    #[test]
    fn digest_is_stable_and_distinguishing() {
        let a = budget_compare_program();
        let b = budget_compare_program();
        assert_eq!(a.digest().expect("a"), b.digest().expect("b"));

        // A different relation is a different program → different H_A.
        let mut nodes = a.nodes().to_vec();
        nodes[2] = Node {
            op: NodeOp::Compare {
                rel: CompareRel::Ult,
            },
            operands: vec![NodeId(0), NodeId(1)],
            width: Width::one(),
        };
        let c = AdmissionProgram::new(a.inputs().to_vec(), nodes, a.outputs().clone())
            .expect("well-formed");
        assert_ne!(a.digest().expect("a"), c.digest().expect("c"));
    }

    #[test]
    fn certificate_recomputes_levels_and_digest() {
        let program = budget_compare_program();
        let cert = program.certify().expect("certify");
        assert_eq!(cert.schema_version, ADMISSION_PROGRAM_SCHEMA_VERSION);
        assert_eq!(cert.node_count, 3);
        assert_eq!(cert.bit_depth, 7);
        assert_eq!(cert.input_width, w(64));
        assert_eq!(cert.digest, program.digest().expect("digest"));
        assert_eq!(cert.nodes.len(), 3);
        assert_eq!(cert.nodes[2].bit_level, 7);
        assert!(cert.nodes[2].single_bit, "Compare yields a predicate bit");
        assert!(!cert.nodes[0].single_bit, "Input yields a width lane");
    }
}
