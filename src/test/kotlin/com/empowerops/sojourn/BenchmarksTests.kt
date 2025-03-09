package com.empowerops.sojourn

import kotlinx.collections.immutable.*
import org.assertj.core.api.Assertions.*
import org.assertj.core.api.Condition
import org.testng.Assert.assertThrows
import org.testng.annotations.Test
import java.util.*

typealias FeasibleRegion = Map<String, ClosedRange<Double>>

operator fun FeasibleRegion.contains(vector: InputVector): Boolean
        = this.entries.all { vector.getValue(it.key) in it.value }

class Benchmarks {

    val RandomSamplingPool1234 = RandomSamplingPool.Factory(Random(1234L), false)
    val RandomWalkingPool1234 = RandomBoundedWalkingImproverPool.Factory(Random(1234L))

    @Test fun `sampling sanity check`() = runTest(RandomSamplingPool1234, SanityCheck)
    @Test fun `random walking sanity check`() = runTest(RandomWalkingPool1234, SanityCheck)
    @Test fun `z3 sanity check`() = runTest(Z3SolvingPool, SanityCheck.copy(targetSampleSize = 100))

    @Test fun `sampling braindead inequalities`() = runTest(RandomSamplingPool1234, BriandeadInequalitySet)
    @Test fun `random walking brainded inequalities with 100 seeds`() = runTest(RandomWalkingPool1234, BriandeadInequalitySet)
    @Test fun `z3 braindead inequalities`() = runTest(Z3SolvingPool, BriandeadInequalitySet.copy(targetSampleSize = 100))

    @Test fun `sampling top-corner-200D inequalities`() {
        runTest(RandomSamplingPool1234, TopCorner200D, allowEmptyResults = true)
    }
    @Test fun `random walking top-corner-200D with one seed`() = runTest(RandomWalkingPool1234, TopCorner200D)
    @Test fun `random walking tough-single-var with no seeds`() = runTest(RandomWalkingPool1234, ToughSingleVar, allowEmptyResults = true)
    @Test fun `z3 top-corner-200D`() = runTest(Z3SolvingPool, TopCorner200D.copy(targetSampleSize = 100))

    @Test fun `sampling on P118`() {
        runTest(RandomSamplingPool1234, P118, allowEmptyResults = true)
    }
    @Test fun `random walking on P118`() {
        runTest(RandomWalkingPool1234, P118.copy(targetSampleSize = 10_000))
    }
    @Test fun `z3 on P118`() = runTest(Z3SolvingPool, P118.copy(targetSampleSize = 100))

    @Test fun `sampling on tough-single-var`() = runTest(RandomSamplingPool1234, ToughSingleVar)
    @Test fun `z3 tough-single-var`(): Unit = TODO("im getting hanging behaviour in sine").also { runTest(Z3SolvingPool, ToughSingleVar.copy(targetSampleSize = 100)) }
    
    @Test(enabled = false) fun generateReportData() {

        val specs = mapOf(
                "P118" to P118,
                "200D corner" to TopCorner200D,
                "Parabola type 0" to ParabolaRootsTypeZero,
                "Parabola type 1" to ParabolaRootsTypeOne,
                "Parabola type 2" to ParabolaRootsTypeTwo,
                "Parabola type 3" to ParabolaRootsTypeThree,
                "Parabola type 4" to ParabolaRootsTypeFour,
                "Parabola type 5" to ParabolaRootsTypeFive
        )

        for((stratName, strategy) in mapOf(
                "Random Sampling" to RandomSamplingPool1234,
                "Improoving" to RandomWalkingPool1234,
                "SMT" to Z3SolvingPool
        )){

            println("strategy: $stratName")
            println()

            for((specName, constraint) in specs.entries){
                
                val constraint = when {
                    strategy == Z3SolvingPool -> constraint.copy(targetSampleSize = 40)
                    constraint == P118 && strategy == RandomWalkingPool1234 -> constraint.copy(targetSampleSize = 100)
                    constraint == TopCorner200D && strategy == RandomWalkingPool1234 -> constraint.copy(targetSampleSize = 50)
                    else -> constraint
                }

                println(specName)
                println()

                val excelResults = ExcelResults()

                (1 .. 10).map {
                    try {
                        runTest(strategy, constraint, excelResults)
                    }
                    catch(ex: NoResultsException){
                        //continue
                    }
                }

                println(excelResults)
                println()
            }
        }
    }



    private fun runTest(
            solverFactory: ConstraintSolvingPoolFactory,
            constraintSpec: ConstraintSet,
            excelResults: ExcelResults? = null,
            allowEmptyResults: Boolean = false
    ): Unit = constraintSpec.run {
        //setup
        val excelResults = excelResults ?: ExcelResults()
        val solver = solverFactory.create(inputs, constraints)

        //act
        val (timeTaken, results) = measureTime { solver.makeNewPointGeneration(targetSampleSize, seeds) }
        val (actualCentroid, actualDispersion) = findDispersion(inputs.map { it.name }, results)

        //assert 1 --publish performance results
        val situationKey = "${solver.name}-${constraintSpec.name}"
        TEAMCITY += "$situationKey-amount" to results.size
        TEAMCITY += "$situationKey-variance" to actualDispersion
        TEAMCITY += "$situationKey-time" to timeTaken

//        if(results.isEmpty()) throw SkipException("$situationKey failed to generate any results")
//        if(results.isEmpty()) {
//            excelResults?.let {
//                it.results += NoResultsGenerated(
//                        requestedPointCount = targetSampleSize,
//                        createdFeasiblePointCount = results.size,
//                        timeTakenMillis = timeTaken
//                )
//            }
//        }

        //assert 2 -- red/green assertions
        for(result in results){
            for(constraint in constraints){
                assertThat(constraint.passesFor(result, tolerance = 1E-12))
                    .describedAs("the result $result passes for ${constraint.expressionLiteral}")
                    .isTrue()
            }
        }
        if( ! allowEmptyResults){
            assertThat(results).describedAs("results generated by pool").isNotEmpty()
        }
        assertThat(results.groupBy { it }.filter { it.value.size >= 2})
            .describedAs("duplicate points generated")
            .isEmpty()

//        assertThat((actualCentroid vecMinus centroid).distance)
//                .describedAs("distance between result centroid $actualCentroid\nand the expected centroid $centroid")
//                .isLessThan(centroid.distance * fudgeFactor)
//
//        assertThat(actualDispersion).isEqualTo(dispersion, Offset.offset(fudgeFactor * dispersion))

        //assert 3 -- publish to excel stuff
        excelResults?.let {
            it.results += SuccessfulExcelResult(
                    requestedPointCount = targetSampleSize,
                    createdFeasiblePointCount = results.size,
                    timeTakenMillis = timeTaken,
                    velocityFeasible = (results.size.toDouble() / timeTaken) * 1000.0,
                    dispersion = actualDispersion,
                    timeToAllRegions = calcPointsTakenUntilAllRegionsSampled(results, constraintSpec),
                    hitPercentage = results.size.toDouble() / targetSampleSize * 100.0
            )

            println(it.toString())
        }


        Unit
    }
}

class NoResultsException(name: String): RuntimeException("'$name' failed to generate any results")

sealed class ExcelResult {
    abstract val requestedPointCount: Int
    abstract val createdFeasiblePointCount: Int
    abstract val timeTakenMillis: Long
    abstract val velocityFeasible: Double
    abstract val dispersion: Double
    abstract val timeToAllRegions: Int?
    abstract val hitPercentage: Double
}
data class SuccessfulExcelResult(
        override val requestedPointCount: Int,
        override val createdFeasiblePointCount: Int,
        override val timeTakenMillis: Long,
        override val velocityFeasible: Double,
        override val dispersion: Double,
        override val timeToAllRegions: Int?,
        override val hitPercentage: Double
): ExcelResult()

data class NoResultsGenerated(
        override val requestedPointCount: Int,
        override val createdFeasiblePointCount: Int,
        override val timeTakenMillis: Long
): ExcelResult() {
    override val velocityFeasible = 0.0
    override val dispersion = 0.0
    override val timeToAllRegions: Int? = null
    override val hitPercentage: Double = 0.0
}

data class ExcelResults(var results: List<ExcelResult> = emptyList()) {
    override fun toString(): String {
        val builder = StringBuilder()
        builder.append("requestedPointCount, createdFeasiblePointCount, timeTaken (ms), velocityFeasible (pt/s), dispersion (distance), triesToAllRegions (count), hitPercentage (percent)")
        builder.append("\n")
        results.joinTo(builder, separator = "\n") { result -> result.run {
            val formattedVelocity = "%.3f".format(velocityFeasible)
            val formattedDispersion = "%.3f".format(dispersion)
            "$requestedPointCount, $createdFeasiblePointCount, $timeTakenMillis, $formattedVelocity, $formattedDispersion, $timeToAllRegions, $hitPercentage"
        }}

        return builder.toString()
    }
}

val ConstraintSolvingPool.name: String get() = javaClass.simpleName

fun expand(inputs: List<String>, values: List<Double>): PersistentList<InputVector> {

    var results = immutableListOf<InputVector>()

    for(index in 0 until values.size step inputs.size) {
        val result = inputs.zip(values.subList(index, index + inputs.size)).toInputVector()
        results += result
    }

    return results
}

fun calcPointsTakenUntilAllRegionsSampled(values: List<InputVector>, constraintSpec: ConstraintSet): Int? {
    var remainingUnsatisfiedRegions = constraintSpec.feasibleRegions

    for((index, candidate) in values.withIndex()){
        val passingRegion = remainingUnsatisfiedRegions.singleOrNull { candidate in it }
        if(passingRegion != null) remainingUnsatisfiedRegions -= passingRegion

        if(remainingUnsatisfiedRegions.isEmpty()) return index + 1
    }

    return null
}

object TEAMCITY {
    operator fun plusAssign(nameValuePair: Pair<String, Any>) {
        println("##teamcity[buildStatisticValue key='${nameValuePair.first}' value='${nameValuePair.second}']")
    }
}