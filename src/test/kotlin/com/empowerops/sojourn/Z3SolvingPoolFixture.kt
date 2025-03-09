package com.empowerops.sojourn

import com.empowerops.babel.BabelCompiler
import com.empowerops.babel.BabelExpression
import kotlinx.collections.immutable.ImmutableList
import kotlinx.collections.immutable.immutableListOf
import org.assertj.core.api.Assertions.*
import org.testng.annotations.Test

class Z3SolvingPoolFixture {

    val compiler = BabelCompiler

    @Test
    fun `simple arithmatic`() = runTest(
            mapOf(
                    "x1" to 0.0 .. 1.0,
                    "x2" to 0.0 .. 1.0,
                    "x3" to 0.0 .. 1.0,
                    "x4" to 0.0 .. 1.0
            ),
            listOf("x2 == x1 + 1/2*x2 - x3 / x4 +/- 0.00001")
    )

    @Test
    fun `roots`() = runTest(
            mapOf(
                    "x1" to 0.0 .. 10.0,
                    "x2" to 0.0 .. 10.0,
                    "x3" to 0.0 .. 10.0,
                    "x4" to 0.0 .. 10.0
            ),
            listOf(
                    "x1 == sqrt(x2) +/- 0.0001",
                    "x3 == cbrt(x4) +/- 0.0001"
            )
    )

    @Test
    fun `power`() = runTest(
            mapOf(
                    "x1" to 0.0 .. 10.0,
                    "x2" to 0.0 .. 10.0
            ),
            listOf("x1 == x2^3 +/- 0.0001")
    )

    @Test fun `power with variable as exponent`() = runTest(
            mapOf(
                    "x5" to 0.0 .. 10.0
            ),
            listOf(
                    "20 > 2^x5" //nope, Z3 wont reason about real-exponents.
            )
    )

    @Test fun `logarithms`() = runTest(
            mapOf(
                    "x1" to 0.0..10.0,
                    "x2" to 0.0..10.0
//                    "x3" to 0.0..10.0,
//                    "x4" to 0.0..10.0,
//                    "x5" to 0.0..10.0,
//                    "x6" to 0.0..10.0
            ),
            listOf(
                "2 < ln(x1)"
//                    "x4 == log(4) +/- 0.0001"
//                "x6 > log(2.0, x5)"
            )
    )

    @Test fun `mod`() = runTest(
            mapOf(
                    "x1" to 0.0..10.0,
                    "x2" to 0.0..10.0,
                    "x3" to 0.0..10.0,
                    "x4" to 0.0..10.0
            ),
            listOf(
                    "x1 % 3.0 >= 2",
                    "x3 == x4 % 4.5 +/- 0.0001"
            )
    )

    @Test fun `mod with symbolic divisor`() = runTest(
            mapOf("x1" to 0.0..10.0),
            listOf("3 > 10 % x1")
    )

    @Test
    fun `constants`() = runTest(
            mapOf("x1" to 0.0 .. 10.0, "x2" to 0.0 .. 10.0),
            listOf("x1 == pi +/- 0.001", "x2 == e +/- 0.001")
    )

    @Test
    fun `sgn`() = runTest(
            mapOf(
                    "x1" to -1.0 .. +1.0, "x2" to -2.0 .. +2.0
            ),
            listOf(
                    "x2 == sgn(x1) +/- 0.001"
            )
    )

    @Test fun `ciel and floor`() = runTest(
            mapOf(
                    "x1" to 0.0 .. 10.0,
                    "x2" to 0.0 .. 10.0,
                    "x3" to 0.0 .. 10.0,
                    "x4" to 0.0 .. 10.0
            ),
            listOf(
                    "x1 > floor(x2)",
                    "x3 > ceil(x4) + floor(x4)"
            )
    )
    
    @Test
    fun `vars`() = runTest(
            mapOf("x1" to -1.0 .. + 1.0, "x2" to -2.0 .. +2.0, "x3" to -2.0 .. +2.0),
            listOf("1.5 == var[1] + var[2]  +/- 0.001", "1.5 == var[2] - var[3] +/- 0.001")
    )

    @Test fun `abs`() = runTest(
            mapOf("x1" to 0.0 .. +1.0, "x2" to -1.0 .. +0.0, "x3" to -2.0 .. -1.0),
            listOf("abs(x1) == 1 +/- 0.001", "abs(x2) == 1  +/- 0.001", "abs(x3) == 1.5  +/- 0.001")
    )

    @Test fun `equality`() = runTest(
            mapOf("x1" to -1.0 .. + 1.0, "x2" to -1.0 .. +1.0),
            listOf("x1 == x2 +/- 0.1")
    )

    @Test fun `sin`() = runTest(
            mapOf("x1" to -3.14 .. +3.14, "y" to 0.9 .. 1.0),
            listOf("sin(x1) <= 0")
    )

    @Test fun `sin of value offset by multiples of pi`() {
        //setup & act
        val (constraints, pool, results) = runTestAct(
            mapOf("theta" to Math.PI .. Math.PI*3, "y" to -1.0..+1.0
//                    "big_theta" to Math.PI*3 .. Math.PI*5),
            ),
            listOf("y > sin(theta)"
//            "y > sin(big_theta)"
            )
        )

        //assert
        // if we made it here we're likely successful,
        // and right now I'm going to blithely accept that a polluted result is fine
        assertThat(results).hasSize(10)
        assertThat(results.filter { ! constraints.passFor(it) })
            .isNotEmpty() //this is OK, the SMT solver generated poitns that dont actually pass the constraints.
                // this does imply the front-end has to filter the results from the pools,
    }

    @Test fun `when given a highly transcendental function should simply drop provided expression`(){
        runTest(
            mapOf("x1" to 0.0 .. 1.0, "x2" to 0.0 .. 1.0),
            listOf("x1 > sin(ln(cos(2.1^x1)))") //TODO: metitarski might give this expr a run for its money...
            // how can i check that there was a transcoding failure?
        )
    }

    private fun runTest(inputs2: Map<String, ClosedRange<Double>>, constraints2: List<String>) {
        val (constraints, pool, results) = runTestAct(inputs2, constraints2)

        //assert
        assertThat(results).describedAs("the number of results the SMT solver was able to generate").hasSize(10)

        val problems= results
                .associate { res ->
                    res to constraints.firstOrNull { ! it.passesFor(res, tolerance = 1E-10) }
                }
                .filterValues { it != null }
                .also { if (it.values.any()) trace { pool.toString() } }
                .mapKeys { (vec, cons) ->
                    vec.filter { cons!!.containsDynamicLookup || it.key in cons!!.staticallyReferencedSymbols }
                }
                .map { (vec, cons) ->
                    "$vec failed '${cons!!.expressionLiteral}' (delta=${cons!!.evaluate(vec)})"
                }

        assertThat(problems).describedAs("input vector and failing constraint").isEmpty()
    }

    private fun runTestAct(
        inputs2: Map<String, ClosedRange<Double>>,
        constraints2: List<String>
    ): Triple<List<BabelExpression>, Z3SolvingPool, ImmutableList<InputVector>> {
        //more-setup
        val inputs = inputs2.map { (key, value) -> InputVariable(key, value.start, value.endInclusive) }

        val constraints = constraints2.map {
            val result = compiler.compile(it)

            result as? BabelExpression ?: throw RuntimeException("compiler failure in $it: $result")
        }

        //act
        val pool = Z3SolvingPool.create(inputs, constraints)
        val results = pool.makeNewPointGeneration(10, immutableListOf())
        return Triple(constraints, pool, results)
    }
}