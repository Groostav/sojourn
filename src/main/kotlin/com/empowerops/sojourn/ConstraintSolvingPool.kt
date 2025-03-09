package com.empowerops.sojourn

import com.empowerops.babel.BabelExpression
import kotlinx.collections.immutable.ImmutableList
import kotlinx.collections.immutable.PersistentList

//typealias InputVector = ImmutableMap<String, Double>

data class InputVariable(val name: String, val lowerBound: Double, val upperBound: Double, val stepsize: Double = 0.0)

interface ConstraintSolvingPoolFactory {
    fun create(inputSpec: List<InputVariable>, constraints: Collection<BabelExpression>): ConstraintSolvingPool
}

interface ConstraintSolvingPool {

    val name: String

    // note: the results may not actually be feasible!
    // at time of writing, there are two reasons a member of the result might not actually be feasible:
    // 1. rounding error
    // 2. SMT solver hit something it couldnt transcode.
    //
    // you must filter this list on the callers side!
    fun makeNewPointGeneration(pointCount: Int, existingPoints: PersistentList<InputVector>): PersistentList<InputVector>
}



