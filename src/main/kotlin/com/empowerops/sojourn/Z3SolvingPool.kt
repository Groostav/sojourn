package com.empowerops.sojourn

import com.empowerops.babel.*
import com.empowerops.babel.BabelParser.*
import com.microsoft.z3.*
import kotlinx.collections.immutable.*
import org.antlr.v4.runtime.RuleContext
import org.antlr.v4.runtime.tree.ParseTree
import org.antlr.v4.runtime.tree.TerminalNode
import java.util.*
import java.util.stream.Stream
import java.lang.Integer.getInteger
import java.lang.UnsupportedOperationException

class Z3SolvingPool private constructor(
    //todo: replace me with Map<String, InputVariable>
    private val inputs: List<InputVariable>,
    private val constraints: Collection<BabelExpression>
): ConstraintSolvingPool {

    override val name: String = "Z3-SMT"

    private val recompiler = BabelCompiler
    private val z3 = Context()
//    private val solver = z3 { Solver(Tactic("qfnra-nlsat")) }

    private var status: Status? = null
    var problem: BabelExpression? = null; private set

    private val solver = z3 {
        Solver().apply {
            setParameters(context.mkParams().apply {
                add("timeout", getInteger("${Z3SolvingPool::class.qualifiedName}.TimeoutMillis") ?: 5_000)
            })
        }
    }
    
    private val inputExprs: Map<String, RealExpr> = z3 {
        //1: input bounds
        inputs.associate { input ->
            val inputExpr = Real(input.name)

            solver += inputExpr gt input.lowerBound.asZ3Real()
            solver += inputExpr lt input.upperBound.asZ3Real()

            input.name to inputExpr
        }
    }

//    val floor: FuncDecl by lazyZ3 { Function("floor", realSort, returnType = realSort) }
    private val sgn: UnaryFunction<ArithExpr, RealExpr> by lazyZ3 { UnaryFunction("sgn", Arith, Real) }
    private val abs: UnaryFunction<ArithExpr, RealExpr> by lazyZ3 { UnaryFunction("abs", Arith, Real) }

    private val mod: BinaryFunction<ArithExpr, ArithExpr, RealExpr> by lazyZ3 { BinaryFunction("mod", Arith, Arith, Real) }
    private val quot: BinaryFunction<ArithExpr, ArithExpr, IntExpr> by lazyZ3 { BinaryFunction("quot", Arith, Arith, Integer) }

    private fun mkModAxioms(X: ArithExpr, k: ArithExpr) = z3 {
        solver.add(
                (k neq 0) implies (0 lte mod(X, k)),
                (k gt 0) implies (mod(X, k) lt k),
                (k lt 0) implies (mod(X, k) lt -k),
                (k * quot(X, k) + mod(X, k)) eq X
        )
    }

    private val vars: UnaryFunction<RealExpr, RealExpr> by lazyZ3 {
        UnaryFunction("var", Real, Real).also {
            for((index, input) in inputs.withIndex()){
                solver += it((index + 1).zr) eq inputExprs.getValue(input.name)
            }
        }
    }

    // cache for bad results from solver.check
    //
    // our strategy is to simply skip constraints that make us UNKNOWN,
    // thus, if we add a constraint that makes us UNKNOWN, we drop it from the set
    // but need to describe to callers that we're now UNKNOWN even if solver.check() returns SAT,
    // because, of course, the solver is not looking at the whole problem anymore.
    // similarly, if it was ever UNSATISFIABLE, then it must always be so.
    private fun downgradeTo(newStatus: Status, constraint: BabelExpression?) {

        val currentStatus = status

        when {
            currentStatus == null || newStatus < currentStatus -> {
                status = newStatus
                problem = constraint
            }
        }
    }

    fun check() = status ?: solver.check().also { downgradeTo(it, null) }

    override fun toString(): String {
        return "SOLVER:\n$solver\nMODEL:${try { solver.model } catch(ex: Z3Exception) { null }}"
    }

    companion object: ConstraintSolvingPoolFactory {

        private val arch = System.getProperty("os.arch")
        private val Z3DepDir = "z3-4.8.5-merged/$arch-win"

        private val NoLibraryPathModifications: Boolean = java.lang.Boolean.getBoolean("${ConstraintSolvingPoolFactory::class.qualifiedName}.NoLibraryPathModifications")

        init {
            val props = System.getProperties() as MutableMap<String, String>
            val oldValue = props["java.library.path"] ?: ""

            if( ! NoLibraryPathModifications){
                // so, if Z3 isnt in the path,
                // and nobody has yet called loadLibrary, then we can hack it in and get Z3 working with no extra config.
                if("deps/$Z3DepDir/bin" !in oldValue){
                    props["java.library.path"] = oldValue + ";deps/$Z3DepDir/bin"
                }

                //... and if we're on java.version 1.X (X is 6, 7, or 8, for 9+ they changed the format)
                // then we can hack this thing in via reflection, and no -Djava.library.path games need to be played by our entry-point.
                if((props["java.version"]?: "").startsWith("1.")) try {
                    val field = ClassLoader::class.java.getDeclaredField("sys_paths").apply {
                        isAccessible = true
                    }
                    field.set(null, null)
                }
                catch(ex: Exception){
                    when(ex){
                        is SecurityException, is NoSuchFieldException -> {
                            //noop, this is expected unhappy path.
                        }
                        else -> throw ex
                    }
                }

                try {
                    for(library in listOf("msvcr120", "vcomp120", "libz3", "libz3java")){
                        System.loadLibrary(library)
                    }
                }
                // else, we're on java 9+, and the user didnt put z3 on the java.library.path,
                // and its not on the default path
                // all we can do is fail with the best error we can
                catch(ex: UnsatisfiedLinkError){
                    System.err.println("failed to load Z3: java.library.path was '$oldValue'")
                    System.err.println("(did you set -Djava.library.path=%PATH%;deps/$Z3DepDir/bin?)")
                    ex.printStackTrace(System.err)
                }
            }
        }

        fun <T> Stream<T>.asSequence(): Sequence<T> = Sequence(this::iterator)

        override fun create(
                inputSpec: List<InputVariable>,
                constraints: Collection<BabelExpression>
        )
                = Z3SolvingPool(inputSpec, constraints).apply { applyConstraints() }
    }

    private fun applyConstraints() = z3 {

        require(solver.check() == Status.SATISFIABLE) { "before adding constraints solver SAT is ${solver.check()}:\n $solver " }

        //2: transcode constraint expressions
        for(constraint in constraints){

            val transcoder = BabelZ3TranscodingWalker()
            try {
                recompiler.compile(constraint.expressionLiteral, transcoder)
            }
            catch(ex: UnsupportedOperationException){
                trace { "failed to transcode '${constraint.expressionLiteral}', will be skipped" }
                downgradeTo(Status.UNKNOWN, constraint)
                continue
            }

            transcoder.requirements.forEach { transcodedConstraintPart ->
                solver.push()
                solver += transcodedConstraintPart

                val newSatState = solver.check()

                if(newSatState != Status.SATISFIABLE){
                    trace { "constraint expr '${constraint.expressionLiteral}' made constraint set $newSatState\n solver is:\n$solver" }

                    downgradeTo(newSatState, constraint)

                    if(newSatState == Status.UNKNOWN){
                        //if its UNSAT, leave it that way, and the caller can ispect the state himself.
                        solver.pop()
                    }
                }
            }
        }
    }

    class UnsatisfiableConstraintsException(solver: String): RuntimeException(solver)

    override fun makeNewPointGeneration(pointCount: Int, existingPoints: PersistentList<InputVector>): PersistentList<InputVector> {
        var pointCount = pointCount

        if(pointCount == 0) {
            trace { "asked SMT for 0 points" }
            return immutableListOf()
        }
        if(existingPoints.isEmpty()){
            trace { "configured SMT to attempt to generate a seed value" }
            pointCount = pointCount.coerceAtLeast(1)
        }

        pointCount = pointCount.coerceAtMost(10)
        //refuse to generate large batches since we're so slow

        solver.push()

        // note: I'm intentionally not negating the existingPoints list.
        // given that random sampling has succeeded, it completely floods the solver.

        var resolved = solver.check()
        downgradeTo(resolved, null)

        if (resolved == Status.UNSATISFIABLE) {
            throw UnsatisfiableConstraintsException(solver.toString())
        }
        if (resolved != Status.SATISFIABLE) {
            trace { "solver state is now: $resolved, will pop and return empty list. Solver:\n$solver" }
            solver.pop()
            return immutableListOf()
        }

        var model = solver.model
        val seed = model.buildInputVector(inputs)

        var results = immutableListOf(seed)

        z3 {

            // reminder: after solving, z3 will generate a function for input vars for solutions
            // (eg decl-fun x3() = 37.5)
            fun makeNegationOf(inputDecl: FuncDecl): BoolExpr{
                val value = model.getConstInterp(inputDecl) as ArithExpr

                val temp = z3.mkAnonRealConst("negate-prev-${inputDecl.name}")
                return (temp eq value) and (Real(inputDecl.name) neq temp)
            }

            for (index in 1 until pointCount) {

                //TODO: use a distence metric + optimization, rather than simple model subtraction.
                //or use a mkTupleSort? maybe just use an array?
                //  val pointCtor = ctx.mkDatatypeSort("point", arrayOf(ctx.mkConstructor("point", "point", arrayOf("x1"), arrayOf(ctx.mkRealSort()), null)))
                //  val p0 = ctx.mkConstDecl("p0", pointCtor)
                //  solver.add(ctx.mkEq(ctx.selec()))

                //negate
                solver += model.constDecls
                    .filter { it.name.toString() in inputs.map { it.name } }
                    .map { func -> makeNegationOf(func) }

                resolved = solver.check()
                if (resolved == Status.UNSATISFIABLE) {
                    //likely our point negation strategy pushed us into UNSAT
                    // TODO: we should be smarter about this, rather than negating blobs
                    // why dont we play games with greater-than and less than?
                    break
                }

                model = solver.model
                val nextResult = model.buildInputVector(inputs)

                results += nextResult
            }

            return results
        }
    }

    private fun <T> Deque<T>.pop(count: Int) = (1 .. count).map { this.pop() }

    inner class BabelZ3TranscodingWalker: BabelParserBaseListener() {

        var requirements: List<BoolExpr> = emptyList()

        //TODO: null safety on push/pop
        val exprs: Deque<ArithExpr> = LinkedList()

        override fun exitBooleanExpr(ctx: BooleanExprContext) = z3 {
            val dk = when {
                ctx.callsBinaryOp() -> {
                    val right = exprs.pop()
                    val left = exprs.pop()

                    requirements += when {
                        ctx.gt() != null -> left gt right
                        ctx.lt() != null -> left lt right
                        //TODO: this is a hack to hedge against for rounding error
                        ctx.gteq() != null -> left gt right
                        ctx.lteq() != null -> left lt right

                        else -> transcodeFailure("unknown: ${ctx.text}")
                    }
                }
                ctx.eq() != null -> {
                    val (offset, right, left) = exprs.pop(3)

                    val (offsetSym, rightSym, leftSym) = listOf(offset, right, left).map {
                        val nextSymbol = z3.mkAnonRealConst()
                        requirements += nextSymbol eq it
                        nextSymbol
                    }

                    requirements += (leftSym gte rightSym - offsetSym) and (leftSym lte rightSym + offsetSym)
                }
                else -> transcodeFailure("unknown: ${ctx.text}")
            }
        }

        override fun exitScalarExpr(ctx: ScalarExprContext) = z3 {

            val transcoded = when {

                ctx.literal() != null || ctx.variable() != null -> null

                ctx.callsDynamicVariableAccess() -> {
                    val index = exprs.pop() as RealExpr
                    vars(index)
                }

                ctx.negate() != null -> -exprs.pop()

                (ctx[0] as? TerminalNode)?.symbol?.type == BabelLexer.OPEN_PAREN -> null

                ctx.unaryFunction() != null -> {
                    val arg = exprs.pop() as RealExpr
                    val operatorText = ctx[0].text

                    val result: ArithExpr = when(operatorText) {
                        "sgn" -> {
                            requirements += listOf(
                                    arg eq 0 implies (sgn(arg) eq 0),
                                    arg gt 0 implies (sgn(arg) eq 1),
                                    arg lt 0 implies (sgn(arg) eq -1)
                            )

                            sgn(arg)
                        }
                        "sqrt" -> {
                            val rooted = z3.mkAnonRealConst(operatorText)
                            requirements += rooted gt 0
                            requirements += arg eq rooted * rooted
                            rooted
                        }
                        "cbrt" -> {
                            val tripleRooted = z3.mkAnonRealConst(operatorText)
                            requirements += arg eq tripleRooted * tripleRooted * tripleRooted
                            tripleRooted
                        }
                    //do a tree-match on expr ^ (1/exprB), then do a `mkMul(*exprB.toArray())`?
                        "log" -> {
                            val logged = z3.mkAnonRealConst(operatorText)
                            requirements += arg eq pow(10, logged)
                            logged
                        }
                        "ln" -> {
                            val lawned = z3.mkAnonRealConst(operatorText)
                            requirements += arg eq pow(E, lawned)
                            lawned
                        }
                        "floor" -> {
                            val floored = z3.mkAnonIntConst(operatorText)
                            val rem = z3.mkAnonRealConst("floor_rem")
                            requirements += listOf(
                                    floored + rem eq arg,
                                    rem gte 0,
                                    rem lt 1
                            )

                            floored
                        }
                        "ceil" -> {
                            val ceiled = z3.mkAnonIntConst(operatorText)
                            val rem = z3.mkAnonRealConst("ceil_rem")
                            requirements += listOf(
                                    ceiled - rem eq arg,
                                    rem gte 0,
                                    rem lt 1
                            )

                            ceiled
                        }
                        "abs" -> {
                            requirements += arg lt 0 implies (abs(arg) eq -1 * arg)
                            requirements += arg gte 0 implies (abs(arg) eq arg)

                            abs(arg)
                        }
                        "sin" -> {
                            val sinned = z3.mkAnonRealConst(operatorText)

                            //using an expansion of sin(x): http://math2.org/math/algebra/functions/sincos/expansions.htm
                            val i3Fac = z3.mkReal(1, /*3!*/ 6)
                            val i5Fac = z3.mkReal(1, /*5!*/ 120)
                            val i7Fac = z3.mkReal(1, 5040)
                            val i9Fac = z3.mkReal(1, 362880)
                            val i11Fac = z3.mkReal(1, 39916800)

                            //this is really hard. I cant get it working with mod.
                            // 1. to use a sin function, though this might have been a name collision
                            // 2. to use and "see-through" a 'mod(arg, 2PI)' call. It simply goes UNKNOWN with no solution. bummer.

//                            val modded = z3.mkAnonRealConst("mod-sin")
//                            val leftParam = arg + PI
//                            val rightParam = 2 * PI
//                            mkModAxioms(leftParam, rightParam)
//                            val modded = mod(leftParam, rightParam) - PI
//                            requirements += listOf(
//                                    (arg gt PI) implies (modded eq (mod(leftParam, rightParam) - PI)),
//                                    (arg lte PI) implies (modded eq arg)
//                            )
//                            val modded = arg + (2*PI)
//                            requirements += (modded eq (arg + 2*PI))
                            val modded = arg
//
//                            requirements += listOf(
//                                    modded lt PI
//                                    modded gt -PI
//                            )

                            requirements += sinned eq (
                                    modded
                                    - (i3Fac * modded * modded * modded)
                                    + (i5Fac * pow(modded, 5))
                                    - (i7Fac * pow(modded, 7))
                                    + (i9Fac * pow(modded, 9))
                                    - (i11Fac * pow(modded, 11))
                            )

//                            requirements += ((modded lte PI) and (modded gte -PI)) implies (sinned eq (
//                                    modded
//                                    - (i3Fac * pow(modded, 3))
//                                    + (i5Fac * pow(modded, 5))
//                                    - (i7Fac * pow(modded, 7))
//                                    + (i9Fac * pow(modded, 9))
//                                    - (i11Fac * pow(modded, 11))
//                            ))
//                            requirements += (modded gt PI) implies (sinned eq (
//                                    (modded - 2*PI)
//                                    - (i3Fac * pow(modded - 2*PI, 3))
//                                    + (i5Fac * pow(modded - 2*PI, 5))
//                                    - (i7Fac * pow(modded - 2*PI, 7))
//                                    + (i9Fac * pow(modded - 2*PI, 9))
//                                    - (i11Fac * pow(modded - 2*PI, 11))
//                            ))
                            //I cant seem to get mod working
//                            requirements += sinned lte 1
//                            requirements += sinned gte -1

                            //TODO: regarding "how does intel do it", https://en.wikipedia.org/wiki/CORDIC

                            sinned
                        }
                        else -> transcodeFailure("not implemented: $operatorText")
                    }

                    result
                }
                ctx.callsBinaryOp() -> {
                    val right = exprs.pop()
                    val left = exprs.pop()

                    when(ctx[1]) {
                        is PlusContext -> left + right
                        is MinusContext -> left - right
                        is MultContext -> left * right
                        is DivContext -> left / right
                        is RaiseContext -> pow(left, right)
                        is ModContext -> {
                            mkModAxioms(left, right)
                            mod(left, right)
                        }
                        else -> transcodeFailure("${ctx[1]}")
                    }
                }
                else -> transcodeFailure("op for ${ctx.text}")
            }

            appendInstruction(transcoded)
        }

        private fun ContextConfigurator.letMod(right: ArithExpr, left: ArithExpr, prefix: String = ""): RealExpr {

            //https://github.com/Z3Prover/z3/issues/557

            val mod = z3.mkAnonRealConst("${prefix}mod")
            val quot = z3.mkAnonIntConst("${prefix}quot")

            val X = left
            val k = right

            requirements += listOf(
                    (k neq 0) implies (0 lte mod),
                    (k gt 0) implies (mod lt k),
                    (k lt 0) implies (mod lt -k),
                    (k neq 0) implies ((k * quot + mod) eq X)
//                    mod gte 0,
//                    mod lt k,
//                    quot neq 0,
//                    quot neq mod,
//                    (k * quot) + mod eq X
            )
            return mod
        }

        private fun appendInstruction(transcoded: Expr?) {
            when (transcoded) {
                null -> { }
                is ArithExpr -> exprs.push(transcoded)
                is BoolExpr -> requirements += transcoded
                else -> transcodeFailure("$transcoded")
            }
        }

        override fun exitVariable(ctx: VariableContext) {
            exprs.push(inputExprs(ctx.text))
        }

        override fun exitLiteral(ctx: LiteralContext) = z3{
            exprs.push(when {
                ctx.FLOAT() != null -> z3.mkReal(ctx.FLOAT().text)
                ctx.INTEGER() != null -> z3.mkReal(ctx.INTEGER().text)
                ctx.PI() != null -> PI
                ctx.EULERS_E() != null -> E
                else -> transcodeFailure("constant ${ctx.text}")
            })
        }

    }

    fun <R> lazyZ3(initializer: ContextConfigurator.() -> R) = lazy { z3.configureReals(initializer) }
}

private fun Model.buildInputVector(inputs: List<InputVariable>): InputVector {
    val valuesByName = constDecls
            .map {
                it.name.toString() to getConstInterp(it)
            }
            .filter {
                it.first in inputs.map { it.name }
            }
            .map {
                val interp = it.second
                val decimal = when (interp) {
                    is RatNum -> interp.numerator.bigInteger.toDouble() / interp.denominator.bigInteger.toDouble()
                    is IntNum -> interp.bigInteger.toDouble()
                    else -> transcodeFailure("cant decode interp $interp")
                }
                it.first to decimal
            }
            .toMap()

    return InputVector(inputs.associate { it.name to (valuesByName[it.name] ?: Double.NaN) })
}


private operator fun RuleContext.get(index: Int): ParseTree = getChild(index)

private inline operator fun <K, V> Map<K, V>.invoke(key: K): V = getValue(key)

private var tempId: Int = 0

private fun Context.mkAnonRealConst(prefix: String = ""): RealExpr = mkRealConst("${if(prefix != "") "$prefix-" else ""}temp-${tempId++}")
private fun Context.mkAnonIntConst(prefix: String = ""): IntExpr = mkIntConst("${if(prefix != "") "$prefix-" else ""}temp-${tempId++}")

private fun transcodeFailure(message: String): Nothing= throw UnsupportedOperationException(message)
