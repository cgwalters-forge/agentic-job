#!/usr/bin/env bash
# Keep shell as a launcher; download/exit-status validation lives in Node.
set -euo pipefail
node "$(dirname "$0")/check.mjs"
