package demo
import scala.util.Try
// A greeter.
class Greeter(name: String) {
  def greet(): String = s"Hi $name"
}
object Main {
  def main(args: Array[String]): Unit = println(new Greeter("x").greet())
}
trait Shape { def area: Double }
case class Pt(x: Int)
enum Color { case Red, Green }
