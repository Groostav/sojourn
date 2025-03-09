package com.empowerops.sojourn

import org.assertj.core.api.Assertions.assertThat
import org.testng.annotations.Ignore
import org.testng.annotations.Test
import java.lang.Math.PI

class LanguageFixture {


    @Test fun `when parsing simple decimal string should properly parse`() = runRatioTest("0.1111", 1111 to 10_000)
    @Test fun `when parsing integer string should properly parse`() = runRatioTest("1111", 1111 to 1)
    @Test fun `when parsing funny trailing-dot integer string should properly parse`() = runRatioTest("1111.", 1111 to 1)
    @Test fun `when parsing funny leading-dot integer string should properly parse`() = runRatioTest(".1111", 1111 to 10_000)
    @Test fun `when parsing rational number string should properly parse`() = runRatioTest("12.34", 1234 to 100)

    private fun runRatioTest(str: String, expectedResult: Pair<Int, Int>) {

        fun String.toIntStrict(): Int = toInt().let { when(it) {
            //TODO, this should probably be smarted to allow for the literal max/min value number
            Int.MAX_VALUE -> throw NumberFormatException("overflow representing $this as integer")
            Int.MIN_VALUE -> throw NumberFormatException("underflow representing $this as integer")
            else -> it
        }}

        //act
        val numerator = str.replace(".", "").toIntStrict()
        val denominator = when {
            '.' !in str -> 1
            else -> Math.pow(10.0, 0.0+str.length - str.indexOf('.') - 1).toInt()
        }

        val res = numerator to denominator

        //assert
        assertThat(res).isEqualTo(expectedResult)
    }

    @Test fun `when casting a large double to integer should throw number format`(){
        assertThat(1E20.toInt()).isEqualTo(Int.MAX_VALUE)

        @Suppress("PLATFORM_CLASS_MAPPED_TO_KOTLIN")
        assertThat((1E20 as java.lang.Number).intValue()).isEqualTo(Int.MAX_VALUE)
    }

    @Test fun `remainder function functions usefully`(){

        val x = (-3.0 * PI) + 0.5

        val y = x % (2.0*PI)

        assertThat(y).isEqualTo(-PI + 0.5)
    }

    @Test fun `when using operator overloading should give you traditional prescedence`(){
        val (a, b, c) = listOf("A", "B", "C").map { MyObj(it) }

        val result = a + b * c

        assertThat(result.str).isEqualTo("(A plus (B times C))")
    }

    data class MyObj(val str: String)
    operator fun MyObj.plus(right: MyObj) = MyObj("($str plus ${right.str})")
    operator fun MyObj.times(right: MyObj) = MyObj("($str times ${right.str})")

    @Test fun `when using extended operator overloading sho                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                    uld give intuitive prescedence`(){
        val (a, b, c) = listOf("A", "B", "C").map { MyObj(it) }

        val result = a + b gt c

        assertThat(result.str).isEqualTo("((A plus B) gt C)")
    }

    //operator fun MyObj.compareTo(right: MyObj): MyObj //no good, compareTo must return an Int, which is too bad.
    data class MyBooleanObj(val str: String)
    infix fun MyObj.gt(right: MyObj) = MyBooleanObj("($str gt ${right.str})")

    @Test fun `when asking for mod should get things`(){
        val x1 = 0.5
        val x2 = 0.5

        val res = x1 > x2 % 2.0

        val x = 4;
    }


    @Test fun `cant call contains on maps`(){
        val map: Map<String, Double> = emptyMap()

//        ("1" to 1.0) in map
        //while there is a contains method on Map, its for keys.
    }

    @Test fun things(){
        val x = PointType.allCases

        val y = 4;
    }

//    @Ignore("pending https://github.com/hrldcpr/pcollections/issues/74")
//    @Test fun `when using a TreePVector should not explode`(){
//        var list = TreePVector.empty<Int>()
//
//        for(i in 0 until 20_000){
//            list = list.plus(i)
//        }
//
//        val res = list.subList(10_000, 20_000)
//
//        require(res.size == 10_000) { "result not correctly shaped" }
//        //pending https://github.com/hrldcpr/pcollections/issues/74
//    }
}

sealed class PointType(val value: Int, val title: String) {
    object point: PointType(1, "خطی")
    object  candlestick: PointType(2, "شمعی")

    companion object {
        val allCases: Array<out PointType> = arrayOf(point, candlestick)
    }
}