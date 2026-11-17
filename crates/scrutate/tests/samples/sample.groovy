// Groovy sample
import java.time.LocalDate
class Greeter {
    String name
    // Greets.
    String greet() { return "Hi ${name}" }
}
def helper(x) { return x * 2 }
println helper(2)
