package com.empowerops.sojourn

import kotlinx.collections.immutable.persistentListOf
import kotlinx.coroutines.flow.take
import kotlinx.coroutines.flow.toList
import kotlinx.coroutines.runBlocking
import org.assertj.core.api.Assertions.assertThat
import org.testng.annotations.Test
import kotlin.system.measureTimeMillis

class IntegrationTests {

    @Test fun `when calling front-end on P118`() = runBlocking {

        val time = measureTimeMillis {
            val results = P118.run {
                makeSampleAgent(inputs, constraints, persistentListOf())
            } as? Satisfiable


            val points = results!!.take(20_000).toList()
            assertThat(points).isEqualTo(20_000)
            assertThat(points.all { P118.constraints.passFor(it) })
        }

        println("took $time ms")

        Unit
    }
}