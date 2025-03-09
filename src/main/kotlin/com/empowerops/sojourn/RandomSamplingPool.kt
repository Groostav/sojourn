package com.empowerops.sojourn

import com.empowerops.babel.BabelExpression
import kotlinx.collections.immutable.*
import java.util.*

const val OVER_SAMPLING_FACTOR = 100

typealias Span = ClosedFloatingPointRange<Double>

class RandomSamplingPool private constructor(
        val inputVariables: List<InputVariable>,
        val constraints: Collection<BabelExpression>,
        val random: Random,
        val adaptive: Boolean
): ConstraintSolvingPool {

    override val name = "${if(adaptive)"Adaptive-" else "Random-"}Sampling"

    var bounds: PersistentMap<String, Span> = persistentMapOf()
        private set

    init {
        for(variable in inputVariables){
            bounds += variable.name to (variable.lowerBound .. variable.upperBound)
        }
    }

    private val Span.length: Double get() = (endInclusive - start).also { require(it >= 0.0) }

    private fun Span.last(chunk: Double): Double{
        require(chunk in 0.0..1.0)
        return endInclusive - (endInclusive - start) * chunk
    }
    private fun Span.first(chunk: Double): Double{
        require(chunk in 0.0..1.0)
        return start + (endInclusive - start) * chunk
    }

    override fun makeNewPointGeneration(pointCount: Int, existingPoints: PersistentList<InputVector>)
            : PersistentList<InputVector> {

        if(adaptive && ! existingPoints.isEmpty()) {
            for (variable in inputVariables) {

                val existingBound = bounds.getValue(variable.name)
                val (minOnVar, maxOnVar) = existingPoints.takeLast(1_000).run {
                    minBy { it[variable.name]!! }!![variable.name]!! to maxBy { it[variable.name]!! }!![variable.name]!!
                }

                val newLowerBound = run {

                    val shift = Math.abs(existingBound.start - minOnVar)

                    if (minOnVar < existingBound.start + shift) {
                        //expand
                        existingBound.start - shift
                    }
                    else {
                        //shrink
                        existingBound.start + shift
                    }
                }

                val newUpperBound = run {

                    val shift = Math.abs(maxOnVar - existingBound.endInclusive)

                    if (maxOnVar > existingBound.endInclusive - shift) {
                        //expand
                        existingBound.endInclusive + shift
                    } else {
                        //shrink
                        existingBound.endInclusive - shift
                    }
                }

                bounds += variable.name to (newLowerBound .. newUpperBound)
            }
        }

        val pointCount = pointCount.coerceAtLeast(10) // always force an attempt at sampling

        var results = persistentListOf<InputVector>()

        for(point in 0 until (pointCount * OVER_SAMPLING_FACTOR)){

            val candidate = makeRandomVector()

            if( ! constraints.passFor(candidate)){
                continue
            }

            results += candidate

            if(results.size >= pointCount){
                break
            }
        }

        return results
    }

    private fun makeRandomVector(): InputVector = bounds
            .mapValues { (key, span) -> random.nextDouble() * (span.endInclusive - span.start) + span.start }
            .toInputVector()

    companion object: ConstraintSolvingPoolFactory {
        override fun create(inputSpec: List<InputVariable>, constraints: Collection<BabelExpression>)
                = RandomSamplingPool(inputSpec, constraints, Random(), false)

    }

    class Factory(val random: Random, val adaptive: Boolean): ConstraintSolvingPoolFactory {
        override fun create(inputSpec: List<InputVariable>, constraints: Collection<BabelExpression>)
                = RandomSamplingPool(inputSpec, constraints, random, adaptive)

    }
}

