// Dart sample
import 'package:demo/util.dart';
class Greeter {
  final String name;
  Greeter(this.name);
  /// Greets.
  String greet() => 'Hi $name';
  static Greeter make() => Greeter('x');
}
mixin Walk { void walk() {} }
enum Color { red, green }
extension Twice on int { int get twice => this * 2; }
String helper(String s) => s.toUpperCase();
void main() { print(Greeter.make().greet()); }
