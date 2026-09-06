# Private ecash runtime review — 2026-09-05

Read-only review of checkpoint 2's in-progress workspace implementation. Two high-priority findings were sent directly to the Wallets coordinator. The original findings below are historical; follow-up status appears at the end. No cluster, native wallet, funded, or model runs were launched by this reviewer; this report is the only reviewer edit.

Scope: controller custody integration, native helper capture/input, persisted execution handles, MCP shared lease annotation, and controller PVC lifecycle. Authorization was assessed against the existing exclusive whole-lab lease; concurrent different-principal endpoint leases are outside this checkpoint.

## P1: transient custody completion failure becomes permanently terminal

Location: `crates/proofstormd/src/native_exec.rs`, terminal supervisor receipt branch, around lines 195–245; `crates/proofstormd/src/main.rs`, terminal-action early return in `reconcile_action`.

After a native producer finishes, `private_transfer::complete` records its source receipt and retrieves the private payload from the pinned pod before committing capture. A temporary Kubernetes transport/API or SQLite error can interrupt this step while the original source payload remains available.

The caller catches this error, adds `private_custody_incomplete; reconcile original operation without replay`, and nevertheless writes a terminal action phase. Future controller passes immediately skip that action. The original source operation already owns the transfer, so another capture cannot replace it. Waiting for or resubmitting the original operation does not perform the advised reconciliation. The same terminalization can prevent attaching a receiver receipt after successful native completion.

Keep custody completion retryable using the persisted execution handle and original receipt, without executing the native command again. Preserve explicit native completion and custody status separately. Define a bounded terminal unknown outcome for genuinely unavailable evidence rather than treating every temporary collection error that way.

Required regression: inject one collection failure after successful native export, reconcile the same action again, and establish exactly one producer invocation, retained source identity/hash, successful custody capture and subsequent authorized delivery. Also cover a temporary receiver-receipt persistence failure. No funding is needed for these synthetic cases.

## P1: recursive volume ownership can invalidate private modes on remount

Location: `charts/proofstorm/templates/deployment.yaml:23`, `private_transfer::path`, and `proofstorm_transfer::Vault::open`.

The deployment supplies `fsGroup: 65532` without a change policy. For volume drivers using kubelet's ownership handling, a remount recursively adds group read/write permissions to files and group execute/setgid to directories. Existing private directories and SQLite files can change from 0700/0600 to 02770/0660. This behavior is explicit in Kubernetes' [`changeFilePermission` and `skipPermissionChange`](https://raw.githubusercontent.com/kubernetes/kubernetes/v1.34.0/pkg/volume/volume_linux.go).

Both the runtime root check and foundation reject those modes. Controller replacement can therefore make persisted custody inaccessible. Since periodic expiry and the lab finalizer also open this path, the impact includes blocked normal teardown. Applicability to the deployed storage driver still requires verification; this is not a claim that a live remount already failed.

Preserve strict inner modes with a suitable ownership/provisioning policy. A correctly provisioned PVC root with `OnRootMismatch` is one option for supporting drivers. Avoid weakening the private-store permission checks to accommodate recursive broadening.

Required regression: populate custody, replace the controller pod so the PVC is remounted, verify inner directory/file modes, then reopen/status/deliver/close using the same vault. Test pod replacement, not only a controller process restart within one mount.

## Review limits and next checkpoint

No additional concrete high-priority authorization or plaintext serialization defect was established in this pass. Private bindings require private output; payload bodies travel through helper transport and custody storage while public requests contain opaque references and metadata. The controller checks the shared lease before custody admission and uses the existing persisted native handle to avoid replaying uncertain starts. These are source observations, not a security certification or deterministic runtime result.

Resolve the two findings and retain the targeted regressions before the checkpoint 2 deterministic handoff. Native transfer validation and any agent fuzzer remain separate, subsequent checkpoints.

Coordinator update after this review: custody completion and private-file retirement failures reportedly now retain the original native handle/receipt, remain Running, and retry within the original start plus command deadline plus 60 seconds. Exhaustion retains the native receipt in a failed action. A targeted regression is still being added. This reviewer has not inspected or tested that follow-up change; the finding remains awaiting verification.

## Follow-up source review

**Custody retry: main defect corrected in source, one related edge still open.** The updated receipt branch now persists the native handle and receipt in Running status and requeues custody/retirement failures within the original command deadline plus 60 seconds. A persisted handle continues to bypass command start. The foundation test `transient_collection_failure_can_resume_custody_without_another_native_export` reopens storage, rejects a second export, and completes capture with the original operation and receipt. Its source was reviewed; it does not inject a controller transport failure or exercise controller status transitions.

The later helper-status failure branch still uses the older deadline plus 30 seconds and calls `patch_action_failure`, which constructs terminal status without the previously recorded native receipt/handle. Thus, after a known terminal receipt has been saved for custody retry, a subsequent status transport failure at deadline plus 31 seconds can terminate recovery early and lose the known completion evidence. Use the retained terminal receipt for custody reconciliation and preserve it if the pod or status endpoint subsequently becomes unavailable. This remaining edge was sent directly to the coordinator. No live custody-retry fault injection is claimed.

**PVC permissions: source fix confirmed; retained local remount evidence supports resolution for this deployment.** The chart now uses `fsGroupChangePolicy: OnRootMismatch`; runtime root and vault checks still require exact 0700/0600 permissions. Reviewed evidence under `dev/wallet-integration-runs/private-transfer-02-20260905/` records different controller pod UIDs before/after replacement. Synthetic capture, delivery, private stdin consumption and release succeeded with the same 576,000-byte payload reference and SHA-256. Both native receipts show exit zero, verified cleanup and retired private files. Successful reopened custody operations enforce the unchanged mode checks; this review did not independently stat the live volume or launch a replacement.

The same run's retained close receipt records verified absence and zero inventory. Its overall outcome remains failed because a later assertion expected cocod balance fields from CDK; the actual recorded CDK balance was 700 sats. Neither the remount result nor that partial native transfer establishes a completed bidirectional gate. The corrected full deterministic rerun remains coordinator-owned and pending at this review checkpoint.

## Second follow-up: known completion preservation

The remaining status-loss edge is now **corrected in the reviewed source**. `cached_native_receipt` reloads the persisted supervisor receipt and clears only previous custody-attempt metadata (`transfer_error`, `private_files_retired`, `transfer`). Reconciliation uses that receipt instead of issuing another helper status command. Known completion therefore cannot enter the old no-receipt timeout simply because a later status poll fails. Terminal custody failure retains the receipt and original native handle.

The pod-loss branch now applies only when no cached receipt exists. With known completion, a changed pod cannot trigger helper cancellation or retirement there; source payload retrieval independently checks the pinned pod UID before helper execution. Existing handles also bypass new-action readiness admission in `main.rs`, preserving owned execution observation when readiness changes. Command start remains outside the entire existing-handle branch.

The new controller unit test checks cached exit/hash preservation, removal of stale custody flags, and refusal to treat a running-only artifact as a terminal receipt. This is useful coverage of the receipt helper, not an injected controller transport/pod-loss transition. Its source was inspected; no test was run by this reviewer and no live retry fault test is claimed. Both review findings now have source fixes; the populated local PVC remount additionally has the retained evidence described above. Final deterministic runtime validation remains pending.
