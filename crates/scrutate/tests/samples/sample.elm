module Demo exposing (greet)

import Html

-- A point.
type alias Point = { x : Int }

type Color = Red | Green

{-| Greets. -}
greet : String -> String
greet name =
    "hi " ++ name

twice : String -> String
twice name =
    greet (greet name)
