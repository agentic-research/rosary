#!/bin/sh
# Wrapper: exec the given command with stdin from /dev/null.
# Used by conductor to spawn agents without interactive stdin.
exec "$@" < /dev/null
