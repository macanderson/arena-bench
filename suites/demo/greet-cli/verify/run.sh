#!/bin/sh
# Held-out verification: never on disk while the agent runs.
set -eu
out=$(./greet.sh world)
[ "$out" = "hello, world" ]
out=$(./greet.sh arena)
[ "$out" = "hello, arena" ]
