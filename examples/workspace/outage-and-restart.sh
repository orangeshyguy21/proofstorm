#!/bin/sh
# The controller releases the partition even if this script exits before healing.
set -eu
: "${PROOFSTORM_CONTROL:?Start this script with a workspace control scope}"

"$PROOFSTORM_CONTROL" workspace call '{"call_id":"outage","operation":{"kind":"network_partition","from_component":"chain","to_component":"scripts","duration_seconds":30}}'
if [ "${CRASH_AFTER_PARTITION:-0}" = 1 ]; then
    printf '%s\n' 'Simulating a script failure while the partition is active' >&2
    exit 23
fi

sleep 5
"$PROOFSTORM_CONTROL" workspace call '{"call_id":"restart","operation":{"kind":"component_restart","component":"chain"}}'
"$PROOFSTORM_CONTROL" workspace call '{"call_id":"heal","operation":{"kind":"network_heal","partition_call_id":"outage"}}'
