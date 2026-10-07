#!/bin/sh
# Stands in for a toolchain install: `sandbox setup` runs it as the sandbox
# user, and the job checks whose the file is.
set -eu
id -un > "$HOME/toolchain-installed-by"
