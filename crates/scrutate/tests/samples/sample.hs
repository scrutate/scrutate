module Demo where

import Data.List (sort)

-- | A point.
data Point = Point Int Int

newtype Age = Age Int
type Name = String

class Shape a where
  area :: a -> Double

instance Shape Point where
  area _ = 0

-- | Greets.
greet :: String -> String
greet n = "hi " ++ n

main :: IO ()
main = putStrLn (greet "x")
