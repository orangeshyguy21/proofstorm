#!/bin/sh
set -eu
if command -v python || command -v python3; then exit 1; fi
test "$(id -u)" = 1000
cdk-cli --version
/opt/proofstorm/driver --self-check
