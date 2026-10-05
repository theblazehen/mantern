#!/usr/bin/env bash
# Crabbox worker: release build with the zlib libmandoc-rs needs.
set -euo pipefail
dev=/nix/store/gqlhr6gyj9py3ibr6qmdk0yv14bpdywz-crabbox-native-development-libraries
export C_INCLUDE_PATH=$dev/include LIBRARY_PATH=$dev/lib LD_LIBRARY_PATH=$dev/lib
cargo build --release 2>&1 | grep -E "^(warning|error)" -A8 || true
