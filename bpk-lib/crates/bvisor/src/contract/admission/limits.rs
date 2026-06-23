//! The structural limits the validator (kernel plan §3) enforces.
//!
//! Frozen FORM — a polynomial node bound and a logarithmic depth bound — with the
//! concrete constants pinned in [`FROZEN_LIMITS`]. The bounds are what make the NC¹
//! claim *checkable* rather than rhetorical: a program over its limits is rejected,
//! so "bounded depth, polynomial size" is a property of every accepted program, not
//! a promise.

use super::program::{ceil_log2, AdmissionProgram, Width, MAX_WIDTH};

/// The structural bounds: a widest-lane cap, a polynomial node bound, and a
/// logarithmic depth bound.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProgramLimits {
    /// The widest admissible single lane.
    pub max_width: u16,
    /// Constant term of the polynomial node bound `node_base + node_per_width·W`.
    pub node_base: u32,
    /// Linear coefficient of the polynomial node bound.
    pub node_per_width: u32,
    /// Coefficient `c` of the depth bound `c·⌈log₂ W⌉ + d`.
    pub depth_coeff: u32,
    /// Constant `d` of the depth bound.
    pub depth_const: u32,
}

/// The frozen limit constants. Provisional values, pinned until a schema bump:
/// generous enough for a 12-membrane admission circuit over capped requirement /
/// evidence / budget inputs, while keeping `|V|` polynomial and `D(A) = O(log W)`.
pub const FROZEN_LIMITS: ProgramLimits = ProgramLimits {
    max_width: MAX_WIDTH,
    node_base: 4096,
    node_per_width: 256,
    depth_coeff: 16,
    depth_const: 32,
};

/// Why a program exceeds the structural limits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum LimitViolation {
    /// A lane is wider than `max_width`.
    WidthExceeded {
        /// The offending width.
        width: u16,
        /// The cap.
        max: u16,
    },
    /// The node count exceeds the polynomial bound `p(W)`.
    NodeCountExceeded {
        /// The node count.
        count: u64,
        /// The bound `p(W)`.
        bound: u64,
    },
    /// The bit-level depth exceeds `c·⌈log₂ W⌉ + d`.
    DepthExceeded {
        /// The measured depth.
        depth: u32,
        /// The bound.
        bound: u32,
    },
}

impl ProgramLimits {
    /// The polynomial node bound `p(W) = node_base + node_per_width·W`, in `u64` so
    /// the product cannot overflow.
    #[must_use]
    pub fn max_nodes(&self, width: Width) -> u64 {
        u64::from(self.node_base) + u64::from(self.node_per_width) * u64::from(width.get())
    }

    /// The depth bound `c·⌈log₂ W⌉ + d`.
    #[must_use]
    pub fn max_bit_depth(&self, width: Width) -> u32 {
        self.depth_coeff
            .saturating_mul(ceil_log2(u32::from(width.get())))
            .saturating_add(self.depth_const)
    }

    /// Check a program against the limits FAIL-CLOSED. Returns the first violation.
    ///
    /// # Errors
    /// The first [`LimitViolation`] found: an over-wide lane, an over-large node
    /// count, or an over-deep circuit.
    pub fn check(&self, program: &AdmissionProgram) -> Result<(), LimitViolation> {
        let width = program.max_width();
        if width.get() > self.max_width {
            return Err(LimitViolation::WidthExceeded {
                width: width.get(),
                max: self.max_width,
            });
        }
        let count = u64::try_from(program.node_count()).unwrap_or(u64::MAX);
        let bound = self.max_nodes(width);
        if count > bound {
            return Err(LimitViolation::NodeCountExceeded { count, bound });
        }
        let depth = program.bit_depth();
        let depth_bound = self.max_bit_depth(width);
        if depth > depth_bound {
            return Err(LimitViolation::DepthExceeded {
                depth,
                bound: depth_bound,
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod limits_tests {
    use super::super::program::{
        ceil_log2, AdmissionProgram, InputDecl, InputSlot, Node, NodeId, NodeOp, Outputs, Width,
    };
    use super::{LimitViolation, ProgramLimits, FROZEN_LIMITS};

    fn w(bits: u16) -> Width {
        Width::new(bits).expect("valid width")
    }

    /// A one-node, 8-bit program (an Input read back as admit predicate would be
    /// ill-typed, so use a 1-bit input for a structurally simple limits subject).
    fn tiny() -> AdmissionProgram {
        AdmissionProgram::new(
            vec![InputDecl {
                width: Width::one(),
            }],
            vec![Node {
                op: NodeOp::Input { slot: InputSlot(0) },
                operands: vec![],
                width: Width::one(),
            }],
            Outputs {
                admit: NodeId(0),
                refusal_code: NodeId(0),
                membranes: vec![],
            },
        )
        .expect("well-formed")
    }

    #[test]
    fn frozen_bounds_have_the_polynomial_and_log_form() {
        let width = w(64);
        assert_eq!(
            FROZEN_LIMITS.max_nodes(width),
            u64::from(FROZEN_LIMITS.node_base)
                + u64::from(FROZEN_LIMITS.node_per_width) * u64::from(width.get()),
        );
        assert_eq!(
            FROZEN_LIMITS.max_bit_depth(width),
            FROZEN_LIMITS.depth_coeff * ceil_log2(u32::from(width.get()))
                + FROZEN_LIMITS.depth_const,
        );
    }

    #[test]
    fn a_tiny_program_is_within_the_frozen_limits() {
        assert!(FROZEN_LIMITS.check(&tiny()).is_ok());
    }

    #[test]
    fn an_over_wide_lane_is_rejected() {
        let strict = ProgramLimits {
            max_width: 0,
            ..FROZEN_LIMITS
        };
        assert!(matches!(
            strict.check(&tiny()),
            Err(LimitViolation::WidthExceeded { .. })
        ));
    }

    #[test]
    fn an_over_large_node_count_is_rejected() {
        let strict = ProgramLimits {
            node_base: 0,
            node_per_width: 0,
            ..FROZEN_LIMITS
        };
        assert!(matches!(
            strict.check(&tiny()),
            Err(LimitViolation::NodeCountExceeded { .. })
        ));
    }
}
