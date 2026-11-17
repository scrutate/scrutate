module Demo
using LinearAlgebra
# A point.
struct Point
    x::Int
end
abstract type Shape end
"Greets."
function greet(name::String)
    return "hi " * name
end
double(x) = 2x
macro show_it(ex) ex end
greet("x")
end
