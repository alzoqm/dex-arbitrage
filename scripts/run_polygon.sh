#!/usr/bin/env bash
set -euo pipefail

cargo run --release -- --chain polygon "$@"
