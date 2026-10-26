#!/usr/bin/env bash
# Keep shell as a launcher; download/exit-status validation lives in
# crates/agentic-job/src/bin/specs-check.rs.
set -euo pipefail
specs=$(cd "$(dirname "$0")" && pwd)
cargo run --quiet --locked --manifest-path "$specs/../Cargo.toml" -p agentic-job --bin specs-check -- "$specs"
