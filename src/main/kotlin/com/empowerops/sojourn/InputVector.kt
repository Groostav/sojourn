package com.empowerops.sojourn

import java.util.AbstractMap

//typealias InputVector = ImmutableMap<String, Double>
//inline fun InputVector(vararg inputValuesByName: Pair<String, Double>): InputVector = immutableHashMapOf(*inputValuesByName)
//inline fun Iterable<Pair<String, Double>>.toInputVector() = toMap().toImmutableHashMap()
//inline fun Map<String, Double>.toInputVector() = toImmutableHashMap()
//operator fun InputVector.div(scalar: Double): InputVector = mapValues { key, value -> value / scalar }
//operator fun InputVector.times(scalar: Double): InputVector = mapValues { key, value -> value * scalar }
//
//infix fun InputVector.vecMinus(other: InputVector): InputVector {
//    require(this.size == other.size)
//
//    val result = InputVector().builder()
//
//    for((key, value) in this){
//        result += key to value - other.getValue(key)
//    }
//
//    return result.build()
//}
//
//infix fun InputVector.vecPlus(other: InputVector): InputVector {
//    require(this.size == other.size)
//
//    this.values
//
//    val result = InputVector().builder()
//
//    for((key, value) in this){
//        result += key to value + other.getValue(key)
//    }
//
//    return result.build()
//}


inline fun Map<String, Double>.toInputVector() = InputVector(this)
inline fun List<Pair<String, Double>>.toInputVector() = InputVector(this)


fun InputVector(vararg values: Pair<String, Double>) = InputVector(
    keys = Array<String>(values.size) { values[it].first },
    values = DoubleArray(values.size) { values[it].second }
)
@JvmName("InputVector_fromPairs")
fun InputVector(values: List<Pair<String, Double>>) = InputVector(
    keys = Array<String>(values.size) { values[it].first },
    values = DoubleArray(values.size) { values[it].second }
)
@JvmName("InputVector_fromMapEntries")
fun InputVector(values: List<Map.Entry<String, Double>>) = InputVector (
    keys = Array<String>(values.size) { values[it].key },
    values = DoubleArray(values.size) { values[it].value }
)

fun InputVector(valuesByName: Map<String, Double>): InputVector {
    val keys = Array<String>(valuesByName.size){ "" }
    val values = DoubleArray(valuesByName.size)

    var index = 0
    for((key, value) in valuesByName){
        keys[index] = key
        values[index] = value
        index += 1
    }

    return InputVector(keys, values)
}


class InputVector : Map<String, Double> {

    companion object {
//        val EMPTY: InputVector = InputVector(keys = TreeSet())
    }

    // TODO hmm.
    // what is the best thing to do for performance here?
    // I dont really want to allocate a million different string arrays that all contain the same info
    // but I also really like the convienience of creating InputVector instances that arent tied to a parent matrix.
    // could use a constant-pool strategy. That would likely avoid the extra allocations.
    // yeah... use something from java util collections that has fast contains and fast index of?
    // pcollections... or kotlinx immutable collections (Q_Q) could have it.

    private val _keys: Array<String>
    private val _values: DoubleArray

    private var __keys: Set<String>? = null
    private var __values: List<Double>? = null
    private var _entries: EntrySet? = null

    internal constructor(keys: Array<String>, values: DoubleArray){
        _keys = keys
        _values = values
    }
    private constructor(keys: Array<String>){
        _keys = keys
        _values = DoubleArray(keys.size)
    }

    override val size: Int get() = keys.size
    override val entries: Set<Map.Entry<String, Double>> get() = _entries ?: EntrySet(this).also { _entries = it }

    override fun containsValue(value: Double) = _values.any { it == value }
    override fun containsKey(key: String): Boolean = key in keys
    override fun get(key: String): Double? = _keys.indexOf(key).let { if(it == -1) null else _values[it] }
    override fun isEmpty() = size == 0
    override val values: List<Double> get() = __values ?: _values.asList().also { __values = it }
    override val keys: Set<String> get() = __keys ?: KeySet(this).also { __keys = it }

    override fun toString(): String = "<${entries.joinToString { (k, v) -> "$k=$v" }}>"



    infix fun vecPlus(right: InputVector): InputVector {
        require(keys == right.keys) { "key miss-match: left=$keys, right=${right.keys}"}

        val result = InputVector(_keys)

        for(index in 0 .. (_values.size - 1)){
            result._values[index] = _values[index] + right._values[index]
        }

        return result
    }

    infix fun vecMinus(right: InputVector): InputVector {
        require(keys == right.keys)

        val result = InputVector(_keys)

        for(index in 0 .. (_values.size - 1)){
            result._values[index] = _values[index] - right._values[index]
        }

        return result
    }

    fun mapValues(transform: (String, Double) -> Double): InputVector {

        val result = InputVector(_keys)

        for((index, key) in keys.withIndex()){
            result._values[index] = transform(key, _values[index])
        }

        return result
    }

    operator fun times(scalar: Double): InputVector {
        val result = InputVector(_keys)

        for(index in 0 .. (_values.size - 1)){
            result._values[index] = _values[index] * scalar
        }

        return result
    }
    inline operator fun div(scalar: Double) = times(1.0/scalar)


    override fun equals(other: Any?): Boolean {
        if (this === other) return true
        if (javaClass != other?.javaClass) return false

        other as InputVector

        if (!_keys.contentEquals(other._keys)) return false
        if (!_values.contentEquals(other._values)) return false

        return true
    }

    override fun hashCode(): Int {
        var result = _keys.contentHashCode()
        result = 31 * result + _values.contentHashCode()
        return result
    }

    class EntrySet(val src: InputVector): AbstractSet<Map.Entry<String, Double>>() {
        override val size = src.size
        override fun contains(element: Map.Entry<String, Double>) = element.key in src.keys

        override fun iterator(): Iterator<Map.Entry<String, Double>> = object: Iterator<Map.Entry<String, Double>> {
            val keysIter = src.keys.iterator()
            var index: Int = 0

            override fun hasNext() = keysIter.hasNext()
            override fun next() = AbstractMap.SimpleEntry(keysIter.next(), src._values[index++])
        }
    }

    class KeySet(val src: InputVector): AbstractSet<String>(){
        override fun iterator(): Iterator<String> = src._keys.iterator()
        override val size = src._keys.size

        //performance sensitive
        @Suppress("LoopToCallChain", "ReplaceRangeToWithUntil")
        override fun contains(element: String): Boolean {
            for(index in 0 .. size-1){
                if(src._keys[index] == element) return true
            }
            return false
        }

        //performance sensitive
        override fun equals(other: Any?): Boolean {
            if(other == null) return false
            if(other is KeySet && other.src._keys === src._keys) return true

            return super.equals(other)
        }
    }
}