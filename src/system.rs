//! A validated set of constraints over a box: the data every strategy reads.
//!
//! Construction is the validation. Every constraint is parsed, checked to be
//! a constraint rather than a bare expression, bound to the box by compiling
//! it — and the tape that compiling produces is kept, since it is the one
//! every feasibility check runs. Two things are derived from the *set* while
//! proving it fits together and kept as well: the drive plan (which
//! coordinates an equality computes from the others, which is what refuses an
//! implicit equality) and the incidence graph between constraints and
//! coordinates. Immutable once built; nothing about it is decided at run time.
//!
//! What the system answers is the simple point questions: is this point in
//! the box and feasible ([`is_feasible`](ConstraintSystem::is_feasible)), and
//! how badly does it miss ([`worst_residual`](ConstraintSystem::worst_residual)).
//! What it does not do is search. The moves along an equality surface, the
//! interval a coordinate may take, the nudge for a solver's witness and the
//! repair of an arbitrary point are the engine's, in `cvg/` and
//! [`FeasibleRegion`](crate::FeasibleRegion), and take a `&ConstraintSystem`.
//!
//! The vocabulary lives here too: a [`Point`] in the box, an [`InputVariable`]
//! bounding one coordinate of it, and a [`ConstraintRef`] naming one
//! constraint the way a caller reads it.

use faer::MatRef;

use crate::cvg::classify;
use crate::cvg::hc4::{self, IntervalTape};
use crate::cvg::incidence::{ConstraintId, Incidence, Row};
use crate::diagnostics::CompilationFailure;
use crate::eval::Gradient;
use crate::{Ast, CompiledExpression, Schema};

/// A point in the input space, one value per variable in declaration order.
///
/// Positional rather than a name-to-value map: the JVM implementation allocated
/// a hash map per candidate inside a loop that oversamples a hundred to one, and
/// the schema already carries the names. It is also the shape a column-major
/// matrix wants, for when evaluation goes batched.
pub type Point = Vec<f64>;

/// One input variable and the range it may take.
#[derive(Debug, Clone, PartialEq)]
pub struct InputVariable {
    pub name: String,
    pub lower_bound: f64,
    pub upper_bound: f64,
}

impl InputVariable {
    #[must_use]
    pub fn new(name: impl Into<String>, lower_bound: f64, upper_bound: f64) -> Self {
        Self {
            name: name.into(),
            lower_bound,
            upper_bound,
        }
    }

    #[must_use]
    pub fn contains(&self, value: f64) -> bool {
        (self.lower_bound..=self.upper_bound).contains(&value)
    }
}

/// One constraint, in a form a caller can read.
///
/// Not a syntax tree. A verdict is something a user reads — in a log line, in a UI
/// telling them their formulation conflicts — and handing back a syntax tree
/// makes them render it themselves. The index is there for anyone who wants to
/// find the original in the list they supplied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConstraintRef {
    /// Position in the list given to [`ConstraintSystem::new`].
    pub index: usize,
    /// The constraint as it was written.
    pub source: String,
}

impl std::fmt::Display for ConstraintRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.source)
    }
}

/// One constraint, as written and as compiled.
///
/// Kept as a pair rather than two parallel lists because everything that
/// indexes one indexes the other by the same [`ConstraintId`], and two lists
/// aligned only by the loop that built them are one refactor from silently
/// disagreeing. The AST is what the classifier reads; the tape is what every
/// feasibility check runs, and its gradient, compiled alongside where the
/// constraint has one, is what a projection's Newton step reads; the interval
/// tape is the same tape over intervals, what every slice and contraction runs.
#[derive(Debug, Clone)]
pub(crate) struct Constraint {
    pub(crate) written: Ast,
    pub(crate) compiled: CompiledExpression,
    pub(crate) intervals: IntervalTape,
}

impl Constraint {
    /// Whether this constraint holds at `point`, and — for a positive
    /// clearance — at `point` moved `clearance * width` either way along each
    /// of the coordinates in `rows`, which are the ones it names.
    ///
    /// Babel's boolean rewrite yields a residual whose sign carries the truth
    /// value: `<= 0` is satisfied, and a residual that cannot be evaluated is
    /// not a pass. The neighbours are along the constraint's own variables
    /// only, because moving any other coordinate leaves its residual where it
    /// was. `neighbour` is scratch the caller lends, equal to `point` on entry
    /// and on return, so a system-wide check allocates once.
    pub(crate) fn holds(
        &self,
        point: &[f64],
        rows: &[Row],
        variables: &[InputVariable],
        clearance: f64,
        neighbour: &mut [f64],
    ) -> bool {
        let passes = |point: &[f64]| {
            self.compiled
                .eval_row(point)
                .ok()
                .is_some_and(|residual| residual <= 0.0)
        };
        if !passes(point) {
            return false;
        }
        if clearance == 0.0 {
            return true;
        }
        for row in rows {
            let coordinate = row.index();
            let variable = &variables[coordinate];
            let step = clearance * (variable.upper_bound - variable.lower_bound);
            if step <= 0.0 {
                continue;
            }
            for sign in [-1.0, 1.0] {
                neighbour[coordinate] = point[coordinate] + sign * step;
                if !passes(neighbour) {
                    neighbour[coordinate] = point[coordinate];
                    return false;
                }
            }
            neighbour[coordinate] = point[coordinate];
        }
        true
    }
}

/// A variable box and the constraints over it, proven to fit together.
///
/// The type exists because these two travelled as parallel slices that nothing
/// validated jointly, and because the properties that matter — how many degrees
/// of freedom are left, which variables another determines — are properties of
/// the *set*, not of any member. "System" is the word for constraints considered
/// together, as in a system of equations.
///
/// Construction is where a constraint naming an undeclared variable is caught,
/// which is what leaves [`solve`](crate::solve)'s `Result` about the
/// search and nothing else.
#[derive(Debug, Clone)]
pub struct ConstraintSystem {
    pub(crate) variables: Vec<InputVariable>,
    /// Every constraint as written and as compiled, at construction. Compiling
    /// is how a constraint is proved to bind, so the tape is kept rather than
    /// made again.
    pub(crate) constraints: Vec<Constraint>,
    /// Which coordinates are computed from the others, when any are: one
    /// plan per way of choosing, empty when nothing is driven. See
    /// [`classify`], and [`classify::tightest`] for how a point picks one.
    pub(crate) plans: Vec<classify::Plan>,
    /// Which constraints name which coordinates, both ways round.
    ///
    /// `slice` narrows one coordinate per move and needs only the constraints
    /// that mention it; without the graph that is a scan of every constraint
    /// each time, which on two hundred variables under two hundred constraints
    /// is forty thousand scans a sweep to do two hundred narrowings.
    pub(crate) incidence: Incidence,
}

/// A system that does not hold together.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum SystemError {
    /// A constraint's text is not a babel expression. Carries every problem the
    /// parser found, with spans, the way [`compile`](crate::compile) would.
    #[error("constraint {constraint} did not parse: {failure}")]
    Unparsable {
        constraint: ConstraintRef,
        failure: CompilationFailure,
    },
    /// A constraint names a variable the box does not declare — by name, or by
    /// position (`var[3]` over two variables). It could never be satisfied, and
    /// saying so once beats saying it on every evaluation — which is what the
    /// JVM implementation did. Carries a problem per name, each with a span at
    /// its first reference, the way [`Unparsable`](Self::Unparsable) does; a
    /// separate arm because it is a different sentence to whoever reads it —
    /// declare a variable, rather than fix a formula.
    #[error("constraint {constraint} did not bind: {failure}")]
    Unbound {
        constraint: ConstraintRef,
        failure: CompilationFailure,
    },
    /// A scalar expression where a constraint was wanted. It has no `<= 0`
    /// reading, so asserting one would invent a constraint nobody wrote.
    #[error("{constraint} is a scalar expression, not a constraint: it has no truth value")]
    NotAConstraint { constraint: ConstraintRef },
    /// `x == sin(x)`, `x2 == x1 + x2/2 - x3/x4` — a variable named on both sides,
    /// so the equality is *implicit* in it: no reading of it yields `v = ...`.
    /// Rows C and F of the equality taxonomy, which turned out to be one thing.
    ///
    /// **A refusal, not a claim that nothing satisfies it.** `sin(x) == x/2` has
    /// three solutions and `x == x*x + 2` is an ordinary quadratic, so
    /// [`Infeasibility::Proved`](crate::Infeasibility::Proved) would be saying something false. What is
    /// true is that nothing here can *drive* such a variable, and a search that
    /// cannot drive it falls back on whatever the sampler manages — which reads
    /// as a capability rather than the gap it is.
    ///
    /// **What is refused is a phrasing.** `x2 == x1 + x2/2` and `x2/2 - x1 == 0`
    /// describe the same set and only the first is implicit, so the message
    /// names the rearrangement rather than only the problem. `cvg_pools::simple_arithmetic`
    /// is the same fixture written the other way round and passes.
    ///
    /// Not to be confused with a *cycle*, which is a mutual dependency between
    /// two equations — `x1 == f(x2)` with `x2 == g(x1)`. `classify::plans` meets
    /// those and drives neither; they are legal, just not reducible.
    ///
    /// Refused at construction because the alternative is worse: a solver call
    /// and several thousand samples before answering `NotFound`, which tells a
    /// caller nothing about what to change.
    #[error(
        "constraint {constraint} is implicit in {variable}: it names {variable} on both sides,          so nothing can solve it for {variable} without rearranging it first. Write {variable}          on one side only - `a == b + a/2` is `a/2 - b == 0`"
    )]
    Implicit {
        constraint: ConstraintRef,
        variable: String,
    },
}

impl ConstraintSystem {
    /// Parses every constraint, checks that each is one, and that each binds
    /// to the box.
    ///
    /// # Errors
    /// [`SystemError`] for the first constraint that does not fit. One rather
    /// than all: an unbound name is nearly always a typo, and a list of
    /// consequences is less use than the cause.
    ///
    /// # Panics
    /// If a variable is named with anything but a legal Babel variable name
    /// ([`is_legal_variable_name`](crate::is_legal_variable_name)), repeats an
    /// earlier variable's name, or ranges over bounds that are not finite with
    /// the lower not above the upper. Equal bounds are fine: they fix the
    /// variable. These are the caller's to check, not verdicts about the
    /// system, and without them the answers are wrong rather than refused:
    /// inverted bounds read as proved infeasible, blaming a constraint.
    pub fn new<S: Into<String>>(
        variables: Vec<InputVariable>,
        constraints: impl IntoIterator<Item = S>,
    ) -> Result<Self, SystemError> {
        for (index, variable) in variables.iter().enumerate() {
            let InputVariable {
                name,
                lower_bound,
                upper_bound,
            } = variable;
            assert!(
                crate::is_legal_variable_name(name),
                "variable {index} is named {name:?}, which is not a legal variable name"
            );
            assert!(
                variables[..index]
                    .iter()
                    .all(|earlier| earlier.name != *name),
                "variable {index} repeats the name {name:?}"
            );
            assert!(
                lower_bound.is_finite() && upper_bound.is_finite(),
                "variable {name:?} ranges over [{lower_bound}, {upper_bound}]; bounds must be finite"
            );
            assert!(
                lower_bound <= upper_bound,
                "variable {name:?} has lower bound {lower_bound} above its upper bound {upper_bound}"
            );
        }
        let schema = Schema::new(variables.iter().map(|input| input.name.clone()));

        let mut resolved = Vec::new();
        let mut compiled = Vec::new();
        for (index, source) in constraints.into_iter().enumerate() {
            let source: String = source.into();
            let named = ConstraintRef {
                index,
                source: source.clone(),
            };
            let constraint = crate::parse(&source).map_err(|failure| SystemError::Unparsable {
                constraint: named.clone(),
                failure,
            })?;
            if !constraint.is_constraint() {
                return Err(SystemError::NotAConstraint { constraint: named });
            }

            // A schema exists here and nowhere earlier, so this is the first
            // moment `var[1]` can be told which variable it means. Resolving it
            // now is why nothing downstream has to: `classify` would refuse the
            // whole constraint rather than reason about it, and `interval`
            // would narrow nothing through it.
            let constraint = crate::frontend::rewrite::resolve_subscripts(constraint, &schema)
                .map_err(|fault| SystemError::Unbound {
                    constraint: named.clone(),
                    failure: crate::frontend::failure(&source, vec![fault]),
                })?;

            // Compiling is the binding check, and the tape it produces is the
            // one every strategy evaluates, so it is kept rather than redone.
            let tape = match crate::eval::bind(&constraint, &schema, Gradient::BestEffort) {
                Ok(tape) => tape,
                Err(failure) => {
                    return Err(SystemError::Unbound {
                        constraint: named,
                        failure,
                    });
                }
            };

            // Last, because "you named a variable that does not exist" is a
            // better message than anything about shape when both are true of
            // the same constraint.
            if let classify::Shape::Implicit { variable } = classify::shape(&constraint) {
                return Err(SystemError::Implicit {
                    constraint: named,
                    variable: constraint.symbols()[variable.index()].clone(),
                });
            }
            resolved.push(constraint);
            compiled.push(tape);
        }

        let plans = classify::plans(&resolved, &schema);

        // What counts as affected by *any* move, whichever coordinate it
        // touched. Both entries here are soundness rather than efficiency.
        let mut always: Vec<ConstraintId> = Vec::new();

        // A computed subscript reads a column chosen by the point, so nothing
        // static says which and no symbol list names it. Skipping such a
        // constraint would mean skipping one that *had* changed.
        always.extend(
            resolved
                .iter()
                .enumerate()
                .filter(|(_, constraint)| constraint.contains_dynamic_lookup())
                .map(|(position, _)| ConstraintId(position)),
        );

        // `retract` rewrites every driven coordinate on every move, so whatever
        // names one of those is in play whichever axis was swept. Read off the
        // naming direction, which is why the graph is built before this.
        let incidence = Incidence::of(&resolved, &schema);
        for driven in plans.iter().flat_map(|plan| plan.driven()) {
            always.extend_from_slice(incidence.naming(Row(*driven)));
        }
        let incidence = incidence.with_always(&always);

        let constraints = resolved
            .into_iter()
            .zip(compiled)
            .map(|(written, compiled)| Constraint {
                intervals: hc4::compile(&written),
                written,
                compiled,
            })
            .collect();

        Ok(Self {
            variables,
            constraints,
            plans,
            incidence,
        })
    }

    #[must_use]
    pub fn variables(&self) -> &[InputVariable] {
        &self.variables
    }

    /// The constraints as written, in the order they were given.
    pub fn constraints(&self) -> impl ExactSizeIterator<Item = &str> {
        self.constraints
            .iter()
            .map(|constraint| constraint.written.source())
    }

    /// Whether a point is inside the box and satisfies every constraint, with
    /// `clearance` box widths of room to spare in every coordinate direction.
    ///
    /// This is the oracle: the one judgement every strategy, and the
    /// caller, agrees on. Inside the box, every constraint's residual `<= 0`,
    /// nothing non-finite. Babel's boolean rewrite yields a residual whose sign
    /// carries the truth value, and a residual that cannot be evaluated is not
    /// a pass.
    ///
    /// At `clearance == 0` that is the whole question. Above it, the box
    /// shrinks by `clearance * width` on each side of every coordinate, and
    /// each constraint must also hold at the point moved that far either way
    /// along each variable *it names* — moving any other coordinate leaves its
    /// residual where it was. That is exactly "survives any per-coordinate
    /// perturbation smaller than the step", which is what a caller's own
    /// normalise-and-back round trip inflicts; on a convex region the axis
    /// neighbours span the whole L1 ball of that radius in box-normalised
    /// coordinates, the ball in the metric [`repair`](crate::FeasibleRegion::repair)
    /// measures distance in. A zero-width coordinate has no room to ask for and
    /// is judged on containment alone.
    ///
    /// One point at a time by nature — a walker cannot propose its next
    /// candidate until it has judged this one — so this runs the per-point
    /// tape; wrapping each point in a one-column matrix cost five times the
    /// evaluation. Thousands of independent candidates at once go through
    /// the batched `feasible_columns`.
    #[must_use]
    pub fn is_feasible(&self, point: &[f64], clearance: f64) -> bool {
        if point.len() != self.variables.len() {
            return false;
        }
        if !(0..self.variables.len()).all(|coordinate| self.within(point, coordinate, clearance)) {
            return false;
        }

        // The scratch the neighbours are built in is allocated only when
        // there will be neighbours: at zero clearance this is the walker's
        // hot loop, a million judgements a run, and an allocation each would
        // show.
        let mut neighbour = if clearance > 0.0 {
            point.to_vec()
        } else {
            Vec::new()
        };
        self.constraints
            .iter()
            .enumerate()
            .all(|(index, constraint)| {
                let rows = self.incidence.rows_of(ConstraintId(index));
                constraint.holds(point, rows, &self.variables, clearance, &mut neighbour)
            })
    }

    /// Whether `point[coordinate]` is inside its bounds with `clearance` box
    /// widths to spare on each side; a zero-width coordinate is judged on
    /// containment alone.
    pub(crate) fn within(&self, point: &[f64], coordinate: usize, clearance: f64) -> bool {
        let variable = &self.variables[coordinate];
        let value = point[coordinate];
        let step = clearance * (variable.upper_bound - variable.lower_bound);
        if step > 0.0 {
            variable.contains(value - step) && variable.contains(value + step)
        } else {
            variable.contains(value)
        }
    }

    /// The gradient of constraint `index`'s residual at `point`, one partial
    /// per coordinate the constraint names in its own order — the order
    /// `incidence.rows_of` gives — or `None` where the constraint has no
    /// derivative or it is not finite at `point`.
    pub(crate) fn gradient(&self, index: usize, point: &[f64]) -> Option<Vec<f64>> {
        self.constraints[index]
            .compiled
            .gradient()?
            .eval_row(point)
            .ok()
    }

    /// The declared box, one interval per variable in row order: what a
    /// contraction starts from and a local solve's unit cube maps onto.
    pub(crate) fn declared(&self) -> Vec<crate::cvg::interval::Interval> {
        self.variables
            .iter()
            .map(|variable| {
                crate::cvg::interval::Interval::new(variable.lower_bound, variable.upper_bound)
            })
            .collect()
    }

    /// How badly the worst constraint is violated, or `None` if the point is
    /// outside the box or a constraint cannot be evaluated there.
    ///
    /// [`is_feasible`](Self::is_feasible) asks a yes-or-no question; this asks
    /// *how far*, which is what the `<= 0` convention makes available: `<= 0`
    /// is feasible, and the size of a positive answer is how badly the point
    /// misses. An optimiser ranking infeasible proposals wants this rather
    /// than the verdict; so does anything stepping downhill toward the region.
    #[must_use]
    pub fn worst_residual(&self, point: &[f64]) -> Option<f64> {
        let in_box = point.len() == self.variables.len()
            && self
                .variables
                .iter()
                .zip(point)
                .all(|(input, value)| input.contains(*value));
        if !in_box {
            return None;
        }
        let mut worst = f64::NEG_INFINITY;
        for constraint in &self.constraints {
            worst = worst.max(constraint.compiled.eval_row(point).ok()?);
        }
        Some(worst)
    }

    /// The columns of `candidates` that are inside the box and satisfy every
    /// constraint, in column order, copied out as points.
    ///
    /// The batched twin of [`is_feasible`](Self::is_feasible) at zero
    /// clearance, for the sources that propose thousands of independent
    /// candidates at once. A candidate whose evaluation faults — `sqrt` of a
    /// negative, a subscript out of range — is one the constraint does not
    /// hold for, exactly as `is_feasible` treats an `Err` per point.
    pub(crate) fn feasible_columns(&self, candidates: MatRef<'_, f64>) -> Vec<Point> {
        let rows = self.variables.len();
        if candidates.nrows() != rows {
            return Vec::new();
        }
        let columns = candidates.ncols();

        let mut pass: Vec<bool> = (0..columns)
            .map(|column| {
                self.variables
                    .iter()
                    .enumerate()
                    .all(|(row, input)| input.contains(candidates[(row, column)]))
            })
            .collect();

        for constraint in &self.constraints {
            constraint.compiled.holds(candidates, &mut pass).expect(
                "`ConstraintSystem::new` proved every constraint binds, and candidates are shaped by the same box",
            );
        }

        (0..columns)
            .filter(|&column| pass[column])
            .map(|column| (0..rows).map(|row| candidates[(row, column)]).collect())
            .collect()
    }

    /// The constraint at `index`, as a caller reads it.
    pub(crate) fn named(&self, index: usize) -> ConstraintRef {
        ConstraintRef {
            index,
            source: self.constraints[index].written.source().to_owned(),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use faer::Mat;

    use crate::cvg::incidence::{ConstraintId, Row};
    use crate::diagnostics::{ProblemKind, Span};
    use crate::{ConstraintSystem, InputVariable, Point, SystemError};

    pub(crate) fn system(inputs: Vec<InputVariable>, sources: &[&str]) -> ConstraintSystem {
        ConstraintSystem::new(inputs, sources.iter().copied()).expect("the fixture binds")
    }

    /// A constraint naming a variable the box does not declare is refused at
    /// construction, and the refusal points at the name.
    #[test]
    fn an_unbound_name_in_a_constraint_points_at_it() {
        let error = ConstraintSystem::new(vec![InputVariable::new("x1", 0.0, 1.0)], ["x1 < x9"])
            .expect_err("x9 is not declared");

        let SystemError::Unbound {
            constraint,
            failure,
        } = error
        else {
            panic!("expected an unbound name, got {error:?}");
        };
        assert_eq!(constraint.index, 0);
        assert_eq!(failure.problems.len(), 1, "{:#?}", failure.problems);
        assert_eq!(
            failure.problems[0].kind,
            ProblemKind::Unbound {
                name: "x9".to_owned()
            }
        );
        assert_eq!(failure.problems[0].span, Span::new(5, 7));
    }

    /// The same refusal by position: `var[3]` over two variables is unbound,
    /// and the caret lands on the subscript.
    #[test]
    fn a_subscript_past_the_box_points_at_it() {
        let error = ConstraintSystem::new(
            vec![
                InputVariable::new("x1", 0.0, 1.0),
                InputVariable::new("x2", 0.0, 1.0),
            ],
            ["x1 < var[3]"],
        )
        .expect_err("there is no third variable");

        let SystemError::Unbound {
            constraint,
            failure,
        } = error
        else {
            panic!("expected an unbound subscript, got {error:?}");
        };
        assert_eq!(constraint.index, 0);
        assert_eq!(failure.problems.len(), 1, "{:#?}", failure.problems);
        assert_eq!(
            failure.problems[0].kind,
            ProblemKind::DynamicIndexOutOfBounds {
                requested_1index: 3,
                available: 2,
            }
        );
        assert_eq!(failure.problems[0].span, Span::new(9, 10));
    }

    fn one_variable(source: &str) -> ConstraintSystem {
        system(vec![InputVariable::new("x1", 0.0, 10.0)], &[source])
    }

    /// Points as a matrix, one column each: the shape the batched judge takes.
    fn points_to_matrix(points: &[Point], rows: usize) -> Mat<f64> {
        Mat::from_fn(rows, points.len(), |row, column| points[column][row])
    }

    /// A computed subscript reads whichever column the point says, so no
    /// static symbol list names the coordinate it depends on. The incidence
    /// graph has to count every coordinate as affecting such a constraint, or
    /// a narrowing that consults only the constraints naming a coordinate
    /// would condition on the wrong ones.
    #[test]
    fn a_computed_subscript_is_never_skipped() {
        let system = system(
            vec![
                InputVariable::new("n", 1.0, 2.0),
                InputVariable::new("x2", -10.0, 10.0),
            ],
            &["var[floor(n)] < 4", "x2 < 3"],
        );
        assert!(
            system.constraints[0].written.contains_dynamic_lookup(),
            "the fixture stopped exercising a computed subscript"
        );

        for coordinate in 0..2 {
            assert!(
                system
                    .incidence
                    .affected(Row(coordinate))
                    .contains(&ConstraintId(0)),
                "coordinate {coordinate} may move the column `var[n]` reads"
            );
        }
    }

    /// The clearance is judged along the variables each constraint names and
    /// against the box: a point one step from a wall fails, one two steps
    /// away passes, and zero clearance is the plain question.
    #[test]
    fn the_clearance_is_a_step_in_every_named_direction() {
        let system = system(
            vec![
                InputVariable::new("x1", 0.0, 1.0),
                InputVariable::new("x2", 0.0, 1.0),
            ],
            &["x1 + x2 < 1"],
        );
        let near_wall = vec![0.45, 0.45]; // 0.1 from the wall along the diagonal, 0.1 along either axis
        assert!(system.is_feasible(&near_wall, 0.0));
        assert!(system.is_feasible(&near_wall, 0.05));
        assert!(
            !system.is_feasible(&near_wall, 0.15),
            "a step of 0.15 along either axis crosses the wall"
        );

        let near_box = vec![0.05, 0.1];
        assert!(system.is_feasible(&near_box, 0.04));
        assert!(
            !system.is_feasible(&near_box, 0.06),
            "the box is a wall too: 0.05 - 0.06 is outside it"
        );
    }

    /// `worst_residual` has to grade, not just judge — a repair steps downhill
    /// and there is no hill in a boolean.
    #[test]
    fn the_worst_residual_is_graded() {
        let system = one_variable("x1 > 4");

        let near = system.worst_residual(&[3.9]).expect("inside the box");
        let far = system.worst_residual(&[1.0]).expect("inside the box");
        assert!(
            near < far,
            "{near} should be a smaller violation than {far}"
        );
        assert!(system.worst_residual(&[5.0]).is_some_and(|r| r <= 0.0));
        assert!(
            system.worst_residual(&[99.0]).is_none(),
            "outside the box is not a residual"
        );
    }

    /// A grid of candidates, some deliberately outside the box.
    fn candidates() -> Vec<Point> {
        let mut points = Vec::new();
        for i in 0..40 {
            let x1 = f64::from(i) * 0.3 - 1.0; // -1 .. 10.7, past both ends of 0..10
            let x2 = f64::from(i % 7) - 3.0;
            points.push(vec![x1, x2]);
        }
        points
    }

    #[test]
    fn batched_judging_agrees_with_per_point_is_feasible() {
        let system = system(
            vec![
                InputVariable::new("x1", 0.0, 10.0),
                InputVariable::new("x2", -5.0, 5.0),
            ],
            &["x1 > 4", "ln(x1) < 2", "x2 * x2 < 5"],
        );

        let points = candidates();
        let matrix = points_to_matrix(&points, 2);
        let batched = system.feasible_columns(matrix.as_ref());
        let one_at_a_time: Vec<Point> = points
            .into_iter()
            .filter(|point| system.is_feasible(point, 0.0))
            .collect();

        assert!(
            !batched.is_empty(),
            "the grid should contain feasible points"
        );
        assert_eq!(batched, one_at_a_time);
    }

    /// `sqrt(x1 - 5)` is NaN for every candidate below five, and the front end
    /// leaves it alone because the root is not the whole side of the comparison
    /// (`ln(x1 - 5) < 0` would be inverted into plain bounds and never fault).
    /// Those candidates are infeasible; the batch still returns the ones above.
    #[test]
    fn a_faulting_candidate_is_infeasible_rather_than_fatal() {
        let system = one_variable("sqrt(x1 - 5) + x1 < 6");

        let points: Vec<Point> = (0..100).map(|i| vec![f64::from(i) * 0.1]).collect();
        let matrix = points_to_matrix(&points, 1);

        // The strict evaluator refuses the batch outright: that is what lenient
        // judging exists to get past.
        let names: Vec<&str> = system.variables().iter().map(|v| v.name.as_str()).collect();
        let strict = crate::compile(system.constraints().next().unwrap(), &names).unwrap();
        assert!(strict.eval(matrix.as_ref()).is_err());

        let feasible = system.feasible_columns(matrix.as_ref());
        assert!(!feasible.is_empty());
        for point in &feasible {
            assert!(point[0] >= 5.0 && point[0] < 6.0, "{point:?}");
        }
        let one_at_a_time: Vec<Point> = points
            .into_iter()
            .filter(|point| system.is_feasible(point, 0.0))
            .collect();
        assert_eq!(feasible, one_at_a_time);
    }

    #[test]
    fn a_candidate_matrix_of_the_wrong_height_yields_nothing() {
        let system = one_variable("x1 > 1");
        let wrong = Mat::from_fn(2, 5, |_, _| 5.0);
        assert!(system.feasible_columns(wrong.as_ref()).is_empty());
    }
}
