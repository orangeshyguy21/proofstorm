#!/usr/bin/env bash
# Run real component/helper contracts locally; no cluster or image publication.
set -euo pipefail
if [[ $# != 4 ]]; then
    printf 'Usage: %s coco|nutshell|cdk DRIVER_IMAGE COMPONENT_IMAGE linux/arm64|linux/amd64\n' "$0" >&2
    exit 2
fi
component=$1
case "$component" in
    coco) argument=COCO_IMAGE ;;
    nutshell) argument=NUTSHELL_IMAGE ;;
    cdk) argument=CDK_IMAGE ;;
    *) printf 'Unsupported component contract\n' >&2; exit 2 ;;
esac
case "$4" in linux/arm64|linux/amd64) ;; *) printf 'Unsupported platform\n' >&2; exit 2 ;; esac
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
docker build --provenance=false --no-cache --platform "$4" --target "$component" \
    --build-arg "DRIVER_IMAGE=$2" --build-arg "$argument=$3" \
    "$root/tests/component-driver"
