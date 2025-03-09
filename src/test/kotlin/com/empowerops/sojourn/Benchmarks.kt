package com.empowerops.sojourn

import com.empowerops.babel.BabelCompilationResult
import com.empowerops.babel.BabelCompiler
import com.empowerops.babel.BabelExpression
import com.empowerops.babel.CompilationFailure
import kotlinx.collections.immutable.ImmutableList
import kotlinx.collections.immutable.PersistentList
import kotlinx.collections.immutable.immutableListOf
import kotlinx.collections.immutable.persistentListOf

private val compiler = BabelCompiler

val SanityCheck = ConstraintSet(
        name = "SanityCheck",
        inputs = listOf(InputVariable("x", -1.0, +1.0)),
        constraints = listOf(
                "x > 0.0"
        ).map { compiler.compile(it).expressionOrThrow() },
        centroid = InputVector("x" to 0.5),
        seeds = immutableListOf(InputVector("x" to 0.5)),
        dispersion = 0.25,
        targetSampleSize = 1_000
)

fun makeParabolicConstraints(offset: Double) = ConstraintSet(
        name = "ParabolicRoots",
        inputs = listOf(InputVariable("x", -5.0, +5.0)),
        constraints = listOf(
                "(x + 2) * (x - 1) == 0 +/- $offset"
        ).map { compiler.compile(it).expressionOrThrow() },
        centroid = InputVector("x" to -0.5),
        dispersion = 1.0,
        targetSampleSize = 20_000,
        seeds = immutableListOf(InputVector("x" to -2.0)),
        feasibleRegions = listOf(
                mapOf("x" to (-2.0 - offset)..(-2.0 + offset)),
                mapOf("x" to (+1.0 - offset)..(+1.0 + offset))
        )
)

val ParabolaRootsTypeZero = makeParabolicConstraints(1.0)
val ParabolaRootsTypeOne = makeParabolicConstraints(0.1)
val ParabolaRootsTypeTwo = makeParabolicConstraints(0.01)
val ParabolaRootsTypeThree = makeParabolicConstraints(0.001)
val ParabolaRootsTypeFour = makeParabolicConstraints(0.0001)
val ParabolaRootsTypeFive = makeParabolicConstraints(0.00001)

val BriandeadInequalitySet = ConstraintSet(
        name = "Braindead",
        inputs = listOf("x1", "x2", "x3", "x4", "x5").map {
            InputVariable(it, 0.0, 1.0)
        },
        constraints = listOf(
                "x1 + x2 > x3",
                "x2 + x3 > x4",
                "x3 + x4 > x5"
        ).map { compiler.compile(it).expressionOrThrow() },
        centroid = InputVector(
                "x1" to 0.559,
                "x2" to 0.619,
                "x3" to 0.561,
                "x4" to 0.506,
                "x5" to 0.440
        ),
        seeds = expand(
                listOf("x1", "x2", "x3", "x4", "x5"),
                OneHundredBraindeadPoints
        ),
        dispersion = 0.579,
        targetSampleSize = 5_000
)

val TopCorner200D = ConstraintSet(
        "Top-corner-200D",
        (1..200).map { "x$it" }.map { InputVariable(it, 10.0, 11.0) },
        (1..200).map { "x$it > 10.5" }.map { compiler.compile(it).expressionOrThrow() },
        (1..200).associate { "x$it" to 10.75 }.toInputVector(),
        seeds = immutableListOf((1..200).associate { "x$it" to 10.75 }.toInputVector()),
        dispersion = 0.95, //TODO: this value doesnt seem correct at all. How is 200 ranges of [10.5..11] variance ~= 1.0?
        // maybe this is some kind of diagonal value? (remember the space is a cube)
        targetSampleSize = 200
)

val ToughSingleVar = ConstraintSet(
        name = "Small single var",
        inputs = listOf(
                InputVariable("x", -3.0, +1.0),
                InputVariable("y", -1.0, +1.0)
        ),
        constraints = listOf(
                //punched this into wolfram, pulls up a very small region.
                //y < sin(x*pi) && y > 1.1* sin(x*pi-0.05) && x > -3.0 && x < 1
                "y < sin(x*pi)",
                "y > 1.1*sin(x*pi-0.5)"
        ).map { compiler.compile(it).expressionOrThrow() },
        centroid = InputVector("x" to 0.0, "y" to 0.0),
        dispersion = 1.0,
        targetSampleSize = 1000
)

val P118 = ConstraintSet(
        name = "P118",
        inputs = listOf(
                InputVariable("x1", lowerBound = 0.0, upperBound = 21.0),
                InputVariable("x2", lowerBound = 0.0, upperBound = 57.0),
                InputVariable("x3", lowerBound = 0.0, upperBound = 16.0),
                InputVariable("x4", lowerBound = 0.0, upperBound = 90.0),
                InputVariable("x5", lowerBound = 0.0, upperBound = 120.0),
                InputVariable("x6", lowerBound = 0.0, upperBound = 60.0),
                InputVariable("x7", lowerBound = 0.0, upperBound = 90.0),
                InputVariable("x8", lowerBound = 0.0, upperBound = 120.0),
                InputVariable("x9", lowerBound = 0.0, upperBound = 60.0),
                InputVariable("x10", lowerBound = 0.0, upperBound = 90.0),
                InputVariable("x11", lowerBound = 0.0, upperBound = 120.0),
                InputVariable("x12", lowerBound = 0.0, upperBound = 60.0),
                InputVariable("x13", lowerBound = 0.0, upperBound = 90.0),
                InputVariable("x14", lowerBound = 0.0, upperBound = 120.0),
                InputVariable("x15", lowerBound = 0.0, upperBound = 60.0)
        ),
        constraints = listOf(
                "0 > -x4+x1-7", "0 > x4-x1-6",
                "0 > -x5+x2-7", "0 > x5-x2-7",
                "0 > -x6+x3-7", "0 > x6-x3-6",
                "0 > -x7+x4-7", "0 > x7-x4-6",
                "0 > -x8+x5-7", "0 > x8-x5-7",
                "0 > -x9+x6-7", "0 > x9-x6-6",
                "0 > -x10+x7-7", "0 > x10-x7-6",
                "0 > -x11+x8-7", "0 > x11-x8-7",
                "0 > -x12+x9-7", "0 > x12-x9-6",
                "0 > -x13+x10-7", "0 > x13-x10-6",
                "0 > -x14+x11-7", "0 > x14-x11-7",
                "0 > -x15+x12-7", "0 > x15-x12-6",

                "0 > -x1-x2-x3+60",
                "0 > -x4-x5-x6+50",
                "0 > -x7-x8-x9+70",
                "0 > -x10-x11-x12+85",
                "0 > -x13-x14-x15+100"
        ).map { compiler.compile(it).expressionOrThrow() },
        centroid = InputVector(),
        dispersion = Double.NaN,
        targetSampleSize = 1000,
        seeds = immutableListOf(InputVector(
                "x1" to 1.0, "x2" to 45.0, "x3" to 15.0, "x4" to 6.0, "x5" to 39.0,
                "x6" to 20.0, "x7" to 11.0, "x8" to 35.0, "x9" to 25.0, "x10" to 16.0,
                "x11" to 40.0, "x12" to 30.0, "x13" to 20.0, "x14" to 46.0, "x15" to 35.0
        ))
)

data class ConstraintSet(
        val name: String,
        val inputs: List<InputVariable>,
        val constraints: List<BabelExpression>,
        val centroid: InputVector,
        val dispersion: Double,
        val targetSampleSize: Int,
        val seeds: PersistentList<InputVector> = persistentListOf(),
        val fudgeFactor: Double = 0.10,
        val feasibleRegions: List<FeasibleRegion> = emptyList()
) {
    init {
        for (outer in feasibleRegions) {
            for (inner in feasibleRegions - outer) {
                for(input: String in inputs.map { it.name }) {
                    val (innerValue, outerValue) = inner[input] to outer[input]
                    if(innerValue == null || outerValue == null) continue

                    require(innerValue.start !in outerValue)
                    require(innerValue.endInclusive !in outerValue)
                }
            }
        }

        seeds.forEach { seed ->
            for(constraint in constraints){
                require(constraint.passesFor(seed)){ "constraint $constraint fails for $seed" }
            }

        }
    }
}

private fun BabelCompilationResult.expressionOrThrow(): BabelExpression = when(this){
    is BabelExpression -> this
    is CompilationFailure -> throw RuntimeException(problems.joinToString("\n") { it.problemValueDescription })
} 