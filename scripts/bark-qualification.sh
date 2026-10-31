#!/usr/bin/env bash
# Bark's entry point for managed candidate qualification; see stack-qualification.sh.
set -Eeuo pipefail
exec bash "$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)/stack-qualification.sh" bark "$@"
