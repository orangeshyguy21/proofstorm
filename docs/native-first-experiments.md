# Native-first experiment validation

Proofstorm composes disposable networks, resolves software versions, manages
lifecycle and faults, and preserves evidence. Native CLIs are the normal way to
operate deployed software. A new experiment should usually require a lab
definition and commands, rather than a new MCP tool.

## Current execution tranche

1. Strengthen evaluation: separate faithful execution, the target-system
   finding, and evidence sufficiency. Manual gates require evidence references.
2. Validate candidate funding and rejection, real proof reservation recovery,
   and live component control in fresh serial K3 sessions.
3. Exercise multi-hop asymmetric liquidity, overlapping faults and selective
   healing, restart during traffic, and liquidity exhaustion/recovery.
4. Review obstacles before changing product abstractions. Subsequent stages
   evaluate held-out briefs, a second wallet through native execution, then the
   model ladder and additional simulation fidelity.

Three varied consecutive candidate funding and reservation-recovery passes are
the foundation target. An attempt with no reserved proofs is inconclusive for
reservation release. Keep failed attempts and their evidence. Do not silently
replace runtime-incompatible candidate code with a release image.

## Execution surface

| Surface | Role |
| --- | --- |
| Live component execution | Native CLI help, queries, and mutations in the real service environment |
| Offline forensics | Source and stored-state inspection; no assumption of live localhost or sockets |
| Proofstorm actions | Provisioning, coordination, faults, lifecycle, and useful portable observations |
| Host shell | Operator harness maintenance, unavailable to experiment agents |

Native command counts and nonzero exits are observations, not blanket failures.
An expected payment refusal can support a finding. A mistaken invocation can
still recover. Review wrong contexts, repeated ineffective commands, unnecessary
wrapper hunting, environment plumbing, and unverified effects. Test a native
mutation followed by a typed observation, including visibility limitations.

The existing typed-only scenarios remain contract regressions. Native-first
scenarios must actually complete native operations and preserve their results;
calling an exec tool alone does not establish a valid experiment.

The `native` toolset is the default in the supplied OpenCode profiles. It keeps the existing cross-phase
control plane and balance/reachability observations, while hiding typed wallet
mutations, routing policy, authentication helpers, the redundant node restart,
the two-LND bootstrap and peer/channel helpers that require it, and the
conservation helper that requires typed wallet-operation evidence. Funding,
peer connection and channel operations use native CLIs in this profile. The
second smoke run exposed a misleading "required first action" bootstrap
description that led a valid one-LND lab to be rebuilt with another node. The
description is corrected in the full profile; the coupled helpers are excluded
from subsequent native runs. Network capability discovery remains available so
agents can distinguish supported faults from unavailable traffic shaping.
The existing `experiment` profile remains the contract comparison. The first
native-surface run uses that existing profile as a baseline; subsequent native
runs use the slim profile and corrected invocation guidance. These combined
changes do not constitute an isolated causal comparison of toolset size.

## MCP surface audit rubric

Retain a typed operation when it earns its place through multi-component
coordination, lifecycle/retry guarantees, portable semantics or observations, or
a recurring error-prone sequence evidenced by transcripts. Single-command
translations are consolidation candidates, not automatic additions. Do not
remove tools merely to hit an arbitrary count; record the commands, context
friction, and wrapper benefit observed before deciding.

Initial families to review: wallet initialize/balance/invoice/pay and channel
policy versus their CLIs. Provisioning/cleanup, candidate resolution/build,
faults, durable execution/waits, and evidence have independent control-plane
responsibilities. Preserve existing contracts while collecting the first round.

## Evidence and cleanup

Record prompt/variant, source revision and dirty diff, binary and harness
digests, topology, image/commit identity, candidate build transformations,
ordered operations, observed timing, and accounting boundaries. Seeds reproduce
inputs, not distributed scheduling. Candidate funding proves runtime viability,
not the behavioral correctness of the PR.

Review experiment validity separately from target property held/violated,
inconclusive, or not applicable. Unknown review gates never become a full pass.
Keep review references to the run's events, operations, and evidence artifacts.

Only one benchmark agent session may run at once. Concurrent traffic inside a
lab is allowed when bounded and measured. Require an idle Proofstorm cluster
before a run and verify no lab namespaces, actions, candidate jobs/pods, or lab
storage remain afterward. Preserve receipts before removing terminal build
resources. Operator cleanup is recorded separately and never substitutes for an
agent teardown pass. Keep the shared controller and Kubernetes services running.

The current NetworkPolicy backend supports partition/heal, not delay/loss.
Verify actual interruption (including established connections), not merely
acceptance of a fault request. Capabilities missing for traffic, timing, or
observations are explicit gaps rather than simulated successes.
