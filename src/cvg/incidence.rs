//! Which constraints name which coordinates, both ways round.
//!
//! A constraint system is a bipartite graph: constraints on one side,
//! coordinates on the other, an edge wherever a constraint names a coordinate.
//! The walker needs it traversed in both directions — given a coordinate, which
//! constraints have anything to say about it, and given a constraint, which row
//! of a point each of its symbols reads — so both are stored.
//!
//! # Why the indices are newtypes
//!
//! Both directions are lists of `usize`, and they mean **different things**:
//! a value in one is a position in the schema, a value in the other is a
//! position in the constraint list. Written as bare integers the transpose
//! reads the same whichever way round it is built —
//! `naming[row].push(constraint)` and `naming[constraint].push(row)` both
//! compile, and only one is right. [`Row`] and [`ConstraintId`] make the
//! compiler read it instead.
//!
//! # On cycles
//!
//! This graph may certainly contain them: `c0 - x1 - c1 - x2 - c0` is two
//! constraints sharing two variables, which is ordinary. The graph where a
//! cycle *means* something is the dependency graph `classify::plans` derives
//! from a set of drives, and it is refused there.

use crate::{Ast, Schema};

/// A coordinate of a point — a position in the bound [`Schema`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct Row(pub(crate) usize);

impl Row {
    pub(crate) const fn index(self) -> usize {
        self.0
    }
}

/// A position in the problem's constraint list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ConstraintId(pub(crate) usize);

impl ConstraintId {
    pub(crate) const fn index(self) -> usize {
        self.0
    }
}

/// The bipartite graph of constraints and coordinates.
#[derive(Debug, Clone)]
pub(crate) struct Incidence {
    /// Per constraint, its own symbol order resolved to rows.
    ///
    /// An AST's `GlobalId` indexes the *constraint's own* symbol list, in first
    /// reference order, where the walker holds a point and thinks in rows. This
    /// is the translation, and it is the same one [`crate::compile`] builds
    /// internally — kept here because narrowing walks the AST while everything
    /// around it speaks in rows.
    rows_of: Vec<Vec<Row>>,
    /// Per row, the constraints naming it.
    naming: Vec<Vec<ConstraintId>>,
    /// Per row, the constraints whose residual can change when it moves.
    ///
    /// [`naming`](Self::naming) plus whatever is in play *whatever* moved — see
    /// [`Incidence::of`]'s `always`.
    affected: Vec<Vec<ConstraintId>>,
}

impl Incidence {
    /// Builds the graph from the constraints, resolved against `schema`.
    ///
    /// [`affected`](Self::affected) starts equal to [`naming`](Self::naming);
    /// [`with_always`](Self::with_always) is what widens it.
    ///
    /// # Panics
    /// If a constraint names something the schema does not, which
    /// `ConstraintSystem::new` has already refused.
    pub(crate) fn of(constraints: &[Ast], schema: &Schema) -> Self {
        let mut rows_of: Vec<Vec<Row>> = Vec::with_capacity(constraints.len());
        let mut naming: Vec<Vec<ConstraintId>> = vec![Vec::new(); schema.names().len()];

        // One edge per (constraint, symbol), recorded in both directions as it
        // is discovered. The two `push`es are what the newtypes are guarding:
        // each list takes only the kind of index it is keyed the other way by.
        for (position, constraint) in constraints.iter().enumerate() {
            let id = ConstraintId(position);
            let mut rows = Vec::with_capacity(constraint.symbols().len());

            for symbol in constraint.symbols() {
                let row = schema
                    .names()
                    .iter()
                    .position(|name| name == symbol)
                    .map(Row)
                    .expect("`ConstraintSystem::new` proved every constraint binds");

                rows.push(row);
                naming[row.index()].push(id);
            }
            rows_of.push(rows);
        }

        Self {
            rows_of,
            affected: naming.clone(),
            naming,
        }
    }

    /// Widens [`affected`](Self::affected) by the constraints that count as
    /// affected by **any** move, whichever coordinate it touched.
    ///
    /// Taken as an argument rather than worked out here because what belongs in
    /// it is the search's business and not the graph's — see
    /// `ConstraintSystem::new`, where both entries are soundness rather than
    /// efficiency. The readers are the repair clamp's per-coordinate filter
    /// and `hc4::slice`.
    pub(crate) fn with_always(mut self, always: &[ConstraintId]) -> Self {
        for constraints in &mut self.affected {
            constraints.extend_from_slice(always);
            constraints.sort_unstable();
            constraints.dedup();
        }
        self
    }

    /// Where each of `constraint`'s symbols reads from, in its own symbol
    /// order — so `rows_of(c)[g]` is the row `GlobalId(g)` means inside `c`.
    pub(crate) fn rows_of(&self, constraint: ConstraintId) -> &[Row] {
        &self.rows_of[constraint.index()]
    }

    /// The constraints that name `row`, and so have something to say about it.
    pub(crate) fn naming(&self, row: Row) -> &[ConstraintId] {
        &self.naming[row.index()]
    }

    /// The constraints whose residual can change when `row` moves.
    pub(crate) fn affected(&self, row: Row) -> &[ConstraintId] {
        &self.affected[row.index()]
    }
}
