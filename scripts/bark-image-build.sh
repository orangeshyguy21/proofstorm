#!/usr/bin/env bash
# Bark's entry point for native candidate builds; see stack-image-build.sh.
set -Eeuo pipefail
exec bash "$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)/stack-image-build.sh" bark "$@"
