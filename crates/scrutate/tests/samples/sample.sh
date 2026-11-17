#!/bin/bash
source ./lib.sh
# Prints a greeting.
greet() {
  echo "hi $1"
}
function build_all {
  make clean
  greet world
}
build_all
