package demo

import kotlin.math.max

// A greeter.
class Greeter(val name: String) {
    fun greet(): String {
        return "Hi " + name
    }
}

object Config {
    val debug = true
}

interface Shape {
    fun area(): Double
}

data class Pt(val x: Int)

fun main() {
    println(Greeter("x").greet())
}
