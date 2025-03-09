package com.empowerops.sojourn

import com.empowerops.babel.BabelCompiler
import com.empowerops.babel.BabelExpression
import com.empowerops.babel.CompilationFailure
import kotlinx.coroutines.channels.take
import kotlinx.coroutines.flow.take
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeoutOrNull

fun main(args: Array<String>) = runBlocking<Unit> {

    // TODO i would really prefer to use a library for this...
    // why not getoptk you dingus?
    var exprArgs = args.toList()

    val targetPointCount = if(args.firstOrNull() == "--targetPointCount"){
        when(val passed = args.drop(1).first()){
            "inf" -> Int.MAX_VALUE
            null -> Int.MAX_VALUE
            else -> passed.toInt()
        }.also {
            exprArgs = exprArgs.drop(2)
        }
    }
    else 10

    val exprs = exprArgs.joinToString(" ").split("&&").map { it.trim() }

    val compiler = BabelCompiler

    val compiled = exprs.map { compiler.compile(it) }
    val constraints = compiled.filterIsInstance<BabelExpression>()

    val inputs = compiled.flatMap { compileResult ->
        when(compileResult){
            is CompilationFailure -> {
                println("compilation failure: ${compileResult.problems.joinToString("\n")}")
                return@runBlocking
            }
            is BabelExpression -> {
                compileResult.staticallyReferencedSymbols.map {
                    InputVariable(it, 0.0, 1.0)
                }
            }
        }
    }.distinct()

    val result = withTimeoutOrNull(20_000) {

        val sampleStream = makeSampleAgent(inputs, constraints)

        println(when(sampleStream){
            is Satisfiable -> "SATISFIABLE"
            is Unknown -> "UNKNOWN: ${sampleStream.problemConstraint?.expressionLiteral}"
            is Unsatisfiable -> "UNSATISFIABLE: ${sampleStream.problemConstraint?.expressionLiteral}"
        })
        if(sampleStream is Worthwhile){
            sampleStream.take(targetPointCount).collect {
                println("$it (PASS=${constraints.passFor(it)})")
            }
        }
    }

    if(result == null) println("timed-out.")

    println("done")
}
