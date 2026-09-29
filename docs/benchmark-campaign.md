# Local benchmark campaigns

`proofstorm-benchmark-campaign` runs a frozen, ordered list of model/task pairs
through the existing acceptance runner. It is a Rust developer CLI; installed
`storm` preview packaging is separate. Model campaigns are opt-in and are never
run by CI. The tests use simulated runners and receipts, without model accounts
or Docker. A one-attempt campaign remains an exploratory comparison, not a
reliable ranking.

## Prepare a plan

Use the account, Docker and checkout prerequisites in
[the benchmark pilot guide](benchmark-preview.md). Commit the intended source
revision yourself before a live campaign: the driver requires a clean checkout
and checks it before every new setup. Build the acceptance runner and campaign
driver from that revision, and retain those binaries unchanged for continuation.

```sh
CARGO_TARGET_DIR=.proofstorm-dev/target cargo build --locked \
  -p proofstorm-acceptance --bin proofstorm-acceptance \
  --bin proofstorm-benchmark-campaign
.proofstorm-dev/target/debug/proofstorm-benchmark-campaign --tasks
```

`--tasks` prints current task versions, contract hashes and model wall deadlines.
It does not start Docker or a model. Put a plan in ignored `dev/`, using absolute
paths, the current revision, SHA-256 hashes of the runner and CLI executables,
and the task hashes printed by `--tasks`. File digests use lowercase hexadecimal
without a prefix; task digests include the printed `sha256:` prefix.
For example, replace every placeholder in this single-slot plan:

```json
{
  "format_version": 1,
  "root": "/absolute/path/to/proofstorm",
  "work": "/absolute/path/to/proofstorm/dev/campaign-001",
  "runner": "/absolute/path/to/proofstorm/.proofstorm-dev/target/debug/proofstorm-acceptance",
  "runner_sha256": "<sha256 of acceptance binary>",
  "revision": "<git rev-parse HEAD>",
  "checkout_home": "/absolute/path/to/proofstorm/.proofstorm-dev/state",
  "gate_timeout_seconds": 1800,
  "max_setup_attempts": 3,
  "runs": [
    {
      "id": "astra-o1",
      "model": "gpt-6-astra",
      "harness": "codex",
      "executable": "/absolute/path/to/codex",
      "executable_sha256": "<sha256 of CLI executable>",
      "task": "benchmark-o1",
      "task_sha256": "<exact hash from --tasks>"
    }
  ]
}
```

Add entries in the intended execution order. IDs must be unique and contain only
letters, digits or hyphens. Supported harnesses are `codex`, `claude-code`, and
`opencode`; tasks are `benchmark-o1` and `benchmark-o5`. Use each CLI's exact model
ID. Authentication uses the existing adapter defaults (Codex file login, Claude
Code login, OpenCode configuration). The plan contains no credentials. Executable
hashes pin the executable file, not an entire CLI installation or its dynamically
loaded dependencies. Model aliases are still subject to provider changes.

The acceptance gate timeout is 1,200–14,400 seconds. Each task also retains its
own model deadline. Neither setting is a hard token or spending cap. All listed
slots can incur model charges.

## Run and continue

```sh
.proofstorm-dev/target/debug/proofstorm-benchmark-campaign \
  --plan "$PWD/dev/campaign-plan.json"
```

The work directory must be new; its parent must exist. It is private (0700), and
logs, receipts and reports are private (0600). A process-held lock prevents two
drivers using the same campaign directory. The driver saves the plan, its own
binary digest and a journal before execution, and gives each setup attempt a
separate directory and log. A model result advances to the next slot, including
a verified zero-score task failure or a task deadline/output-limit failure.

An infrastructure failure stops the campaign with a nonzero exit. Inspect
`report.md`, `results.json`, `summary.json`, `progress.json`, and the referenced
attempt evidence before explicitly continuing:

```sh
.proofstorm-dev/target/debug/proofstorm-benchmark-campaign \
  --plan "$PWD/dev/campaign-plan.json" --resume
```

Continuation rechecks the frozen plan, binaries, clean source revision and prior
receipts. It never repeats a model launch. A completed receipt left by an
interrupted driver is recovered without launching that slot again. Altered or
unverifiable evidence blocks continuation.

Only a verified pre-model setup failure may receive a fresh setup directory:
no model preparation or launch artifacts, no gates started, preservation checked,
and either owned cleanup passed or setup failed in the tools stage before a
runtime receipt existed. Setup retries require `--resume` and are capped by
`max_setup_attempts` (1–3 total per slot). They have no model score and do not
increase the model launch count. A failed cluster creation without verified
cleanup is not safe to continue.

All three model adapters write a durable launch-intent marker immediately before
starting the model-capable CLI. A marker without a trustworthy result means the
model may have run; it cannot be retried by continuation. Provider/harness errors,
missing results, failed preservation or failed cleanup require inspection and
stop the remaining slots. Do not delete markers or rewrite the journal to bypass
this rule. Raw grader output is retained even when the campaign excludes its
score from comparison.

Ctrl-C or SIGTERM requests cancellation and lets the acceptance runner finish
its owned cleanup before the driver exits. Avoid force-killing that cleanup.
New Docker containers, networks and volumes without Proofstorm ownership are
reported without invalidating the run. Ordinary unrelated services, including
Compose services, can be started during a campaign. Installation labels and
reserved current or legacy Proofstorm/k3d names prevent an ownership exemption;
missing ownership evidence fails verification. Existing resources, configuration,
and cleanup of benchmark-owned resources remain strict, with only the existing
preobserved lifecycle exceptions. The runner never deletes unrelated additions.

Acceptance receipts record `preservation_policy`, `preservation_baseline_additions`
and `preservation_additions` (counts by resource type); private snapshots retain
identities. Networks and volumes include creation and ownership observations so
a replacement cannot hide behind the same volume name. Continuation independently
rechecks snapshots and addition counts using the recorded policy. Historical
receipts without this policy keep their original strict interpretation.

## Pinned tool transfers

Bootstrap retries failed transfers up to three times, including partial-transfer
and TLS failures. Each transfer has a 15-second connection timeout, 60-second
total timeout and 65-second process deadline. It discards partial bytes before
retrying. Archive and extracted-executable checksums are still mandatory;
checksum failures are not transfer retries and nothing is installed on mismatch.
Already installed verified tools are reused within their installation. There is
no new global cache.
