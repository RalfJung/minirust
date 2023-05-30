#!/bin/bash
set -ex

# Fixed specr-transpile version
VERSION="0.1.16"

cargo install "specr-transpile@${VERSION}"
specr-transpile specr.toml

(cd tooling/gen-minirust; RUSTFLAGS="-D warnings" cargo build)
(cd tooling/minitest; cargo test)
