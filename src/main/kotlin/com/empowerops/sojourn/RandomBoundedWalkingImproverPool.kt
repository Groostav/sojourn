package com.empowerops.sojourn

import com.empowerops.babel.BabelExpression
import kotlinx.collections.immutable.*
import kotlinx.collections.immutable.immutableListOf
import java.util.*

class RandomBoundedWalkingImproverPool private constructor(
        val inputVariables: List<InputVariable>,
        val constraints: Collection<BabelExpression>,
        val random: Random
): ConstraintSolvingPool {

    override val name = "Improver"

    override fun makeNewPointGeneration(pointCount: Int, existingPoints: PersistentList<InputVector>): PersistentList<InputVector> {

        if(existingPoints.isEmpty()) {
            trace { "improver called without seeds" }
            return immutableListOf()
        }

        val pointCount = pointCount.coerceAtLeast(10)

        var results = immutableListOf<InputVector>()
        val srcPool = existingPoints.takeLast(1_000)

        for(pointIdx in 0 until pointCount) {
            val base = srcPool[random.nextInt(srcPool.size)]

            val direction = randomUnitVector(inputVariables.map { it.name })

            var offsetCandidate = makeBoundedOffset(base, direction)

            //TODO: this also doesnt consider "holes" or multi-zones along the vector
            var changeFactor = 0.5
            for(step in 0 until 20){

                val candidate = base vecPlus offsetCandidate
                val isInSpace = constraints.passFor(candidate) && inputVariables.canProduce(candidate)
                val stepDirection = if(isInSpace) +1.0 else -1.0
                val offset = offsetCandidate * changeFactor * stepDirection
                offsetCandidate = offsetCandidate vecPlus offset
                changeFactor /= 2.0

                //rather than build the candidate here, i could just use a N-step bsearch (eg 20 step) to find the edge,
                //then interpolate linearly between the base and the edge.
            }

            val result = base vecPlus (offsetCandidate * random.nextDouble())
            if(constraints.passFor(result) && inputVariables.canProduce(result)){
                results += result
            }
        }

        return results
    }

    private fun makeBoundedOffset(base: InputVector, direction: InputVector): InputVector {

        var factor = Double.POSITIVE_INFINITY

        for(inputVar in inputVariables){
            val candidate = inputVar.run {
                if(direction[name]!! > 0.0) (upperBound - base[name]!!) / direction[name]!!
                else (base[name]!! - lowerBound) / direction[name]!!
            }
            factor = Math.min(Math.abs(factor), Math.abs(candidate))
        }

        return direction * factor
    }


    private fun randomUnitVector(inputs: List<String>): InputVector {

        val unnormalized = (0 until inputs.size).map { random.nextDouble()*2.0 - 1.0 }
        val length = Math.abs(unnormalized.sum())

        val normalized = unnormalized.map { it / length }

        return inputs.zip(normalized).toInputVector()
    }

    companion object: ConstraintSolvingPoolFactory {
        override fun create(inputSpec: List<InputVariable>, constraints: Collection<BabelExpression>)
                = RandomBoundedWalkingImproverPool(inputSpec, constraints, Random())

    }

    class Factory(val random: Random): ConstraintSolvingPoolFactory {
        override fun create(inputSpec: List<InputVariable>, constraints: Collection<BabelExpression>)
                = RandomBoundedWalkingImproverPool(inputSpec, constraints, random)

    }
}