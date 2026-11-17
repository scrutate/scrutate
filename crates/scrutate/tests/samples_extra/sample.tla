---- MODULE Counter ----
EXTENDS Naturals
VARIABLE count

Init == count = 0

Step(n) == n + 1

Next == count' = Step(count)

Spec == Init /\ [][Next]_count
====
