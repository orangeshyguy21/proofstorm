use super::*;

fn plan(work: &Path, count: usize) -> Plan {
    save(&work.join("journal.json"), &json!([])).unwrap();
    Plan {
        format_version: 1,
        root: work.into(),
        work: work.into(),
        runner: work.join("runner"),
        runner_sha256: "runner-digest".into(),
        revision: "source-revision".into(),
        checkout_home: work.join("checkout"),
        gate_timeout_seconds: 4200,
        max_setup_attempts: 3,
        runs: (0..count)
            .map(|index| Run {
                id: format!("run-{index}"),
                model: format!("model-{index}"),
                harness: "codex".into(),
                executable: work.join("codex"),
                executable_sha256: "cli-digest".into(),
                task: "benchmark-o1".into(),
                task_sha256: contracts()["benchmark-o1"]["sha256"]
                    .as_str()
                    .unwrap()
                    .into(),
            })
            .collect(),
    }
}

fn receipt(plan: &Plan, entry: &Entry, model: bool) {
    let work = plan.attempt_path(entry);
    fs::create_dir(&work).unwrap();
    fs::create_dir(work.join("state")).unwrap();
    let snapshot = json!({"containers":{},"networks":[],"volumes":[],"configuration_sha256":{}});
    for name in ["preservation-before.json", "preservation-after.json"] {
        save(&work.join(name), &snapshot).unwrap();
    }
    save(&work.join("acceptance.json"), &json!({"setup":if model {"passed"} else {"failed"},"cleanup":if model {"passed"} else {"not_run"},"preservation":"passed","preservation_exclusions":{},"gates":[]})).unwrap();
    if !model {
        save(
            &work.join("state/setup-progress.json"),
            &json!({"stage":"tools","status":"failed"}),
        )
        .unwrap();
        return;
    }
    let run = &plan.runs[entry.run];
    save(&work.join("benchmark-task.json"), &json!(task::o1())).unwrap();
    save(&work.join("model-launch.json"), &json!({"state":"launch_requested","model":run.model,"task":task::o1().id,"task_version":task::o1().version})).unwrap();
    save(&work.join("benchmark-manifest.json"), &json!({"model_requested":run.model,"harness":run.harness,"source_revision":plan.revision,"source_dirty":false,"runner_sha256":plan.runner_sha256,"task":task::o1()})).unwrap();
    for name in [
        "benchmark-context.json",
        "benchmark-attempt.json",
        "harness-outcome.json",
        "normalized-calls.json",
    ] {
        save(&work.join(name), &json!({})).unwrap();
    }
    let mut evidence = serde_json::Map::new();
    for name in crate::benchmark::EVIDENCE_FILES {
        if work.join(name).exists() {
            evidence.insert((*name).into(), json!(hash(&work.join(name)).unwrap()));
        }
    }
    // A legitimate model failure is zero, but is still a completed model slot.
    let mut result = crate::benchmark::score::grade(
        task::o1(),
        &json!({}),
        &[],
        Some(1.0),
        "completed",
        true,
        Some(true),
    );
    result["evidence_sha256"] = json!(evidence);
    save(&work.join("benchmark-result.json"), &result).unwrap();
}

static NOT_CANCELLED: AtomicBool = AtomicBool::new(false);

fn addition_receipt(plan: &Plan, entry: &Entry, model: bool, mode: &str) {
    receipt(plan, entry, model);
    let work = plan.attempt_path(entry);
    let before = json!({"format_version":2,"containers":{},"networks":{},"volumes":{},"configuration_sha256":{}});
    let mut after = before.clone();
    after["containers"]["other"] = json!({"name":"/other","owner":null,"cluster":""});
    if mode == "owned" {
        after["containers"]["other"]["owner"] = json!("this-run");
    }
    save(&work.join("preservation-before.json"), &before).unwrap();
    save(&work.join("preservation-after.json"), &after).unwrap();
    let path = work.join("acceptance.json");
    let mut acceptance = read(&path).unwrap();
    acceptance["preservation_policy"] = json!(crate::preservation::ADDITIONS_POLICY);
    acceptance["preservation_additions"] = json!({"containers":1,"networks":0,"volumes":0});
    match mode {
        "cleanup-failed" => acceptance["cleanup"] = json!("failed"),
        "wrong-count" => acceptance["preservation_additions"]["containers"] = json!(0),
        "unknown-policy" => acceptance["preservation_policy"] = json!("unknown"),
        "legacy" => {
            acceptance
                .as_object_mut()
                .unwrap()
                .remove("preservation_policy");
        }
        _ => {}
    }
    save(&path, &acceptance).unwrap();
    if model {
        let path = work.join("benchmark-result.json");
        let mut result = read(&path).unwrap();
        result["evidence_sha256"]["acceptance.json"] =
            json!(hash(&work.join("acceptance.json")).unwrap());
        save(&path, &result).unwrap();
    }
}

#[test]
fn reported_unrelated_additions_allow_results_and_safe_setup_continuation() {
    let dir = tempfile::tempdir().unwrap();
    let plan = plan(dir.path(), 2);
    assert!(
        drive(
            &plan,
            &NOT_CANCELLED,
            || Ok(()),
            |entry| {
                addition_receipt(&plan, entry, false, "allowed");
                Ok(Some(1))
            }
        )
        .is_err()
    );
    drive(
        &plan,
        &NOT_CANCELLED,
        || Ok(()),
        |entry| {
            addition_receipt(&plan, entry, true, "allowed");
            Ok(Some(0))
        },
    )
    .unwrap();
    drive(
        &plan,
        &NOT_CANCELLED,
        || panic!("completed"),
        |_| panic!("no repeat"),
    )
    .unwrap();
    assert_eq!(
        read(&plan.work.join("summary.json")).unwrap()["model_results"],
        2
    );
}

#[test]
fn additions_do_not_relax_cleanup_ownership_or_recorded_policy_verification() {
    for mode in [
        "owned",
        "cleanup-failed",
        "wrong-count",
        "unknown-policy",
        "legacy",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let plan = plan(dir.path(), 1);
        assert!(
            drive(
                &plan,
                &NOT_CANCELLED,
                || Ok(()),
                |entry| {
                    addition_receipt(&plan, entry, true, mode);
                    Ok(Some(0))
                }
            )
            .is_err(),
            "{mode}"
        );
        assert!(drive(&plan, &NOT_CANCELLED, || Ok(()), |_| panic!("no retry")).is_err());
        assert_eq!(
            read(&plan.work.join("results.json")).unwrap()[0]["score"],
            Value::Null
        );
    }
}

#[test]
fn shared_host_activity_is_rechecked_and_does_not_relax_cleanup_or_ownership() {
    for mode in [
        "allowed",
        "cleanup-failed",
        "wrong-count",
        "owned",
        "legacy",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let plan = plan(dir.path(), 1);
        let result = drive(
            &plan,
            &NOT_CANCELLED,
            || Ok(()),
            |entry| {
                receipt(&plan, entry, true);
                let work = plan.attempt_path(entry);
                let before = json!({"format_version":2,"containers":{"orchard":{"name":"/orchard","owner":null,"cluster":""}},"networks":{},"volumes":{},"configuration_sha256":{}});
                let mut after = before.clone();
                after["containers"] = json!({});
                if mode == "owned" {
                    after["containers"]["leak"] =
                        json!({"name":"leak","owner":"this-run","cluster":""});
                }
                save(&work.join("preservation-before.json"), &before)?;
                save(&work.join("preservation-after.json"), &after)?;
                let mut acceptance = read(&work.join("acceptance.json"))?;
                acceptance["preservation_policy"] = json!(crate::preservation::shared::POLICY);
                acceptance["shared_host_activity"] =
                    json!({"containers":{"added":0,"removed":1,"changed":0}});
                match mode {
                    "cleanup-failed" => acceptance["cleanup"] = json!("failed"),
                    "wrong-count" => {
                        acceptance["shared_host_activity"]["containers"]["removed"] = json!(0);
                    }
                    "legacy" => {
                        acceptance["preservation_policy"] =
                            json!(crate::preservation::ADDITIONS_POLICY);
                    }
                    _ => {}
                }
                save(&work.join("acceptance.json"), &acceptance)?;
                let mut grade = read(&work.join("benchmark-result.json"))?;
                grade["evidence_sha256"]["acceptance.json"] =
                    json!(hash(&work.join("acceptance.json"))?);
                save(&work.join("benchmark-result.json"), &grade)?;
                Ok(Some(0))
            },
        );
        assert_eq!(result.is_ok(), mode == "allowed", "{mode}: {result:?}");
        let replay = drive(
            &plan,
            &NOT_CANCELLED,
            || Ok(()),
            |_| panic!("no model replay"),
        );
        assert_eq!(replay.is_ok(), mode == "allowed");
    }
}

#[test]
fn fixed_order_includes_zero_scores_and_resume_never_repeats_models() {
    let dir = tempfile::tempdir().unwrap();
    let plan = plan(dir.path(), 3);
    plan.validate().unwrap();
    let mut order = Vec::new();
    drive(
        &plan,
        &NOT_CANCELLED,
        || Ok(()),
        |entry| {
            order.push(entry.run);
            receipt(&plan, entry, true);
            Ok(Some(0))
        },
    )
    .unwrap();
    assert_eq!(order, vec![0, 1, 2]);
    drive(
        &plan,
        &NOT_CANCELLED,
        || panic!("no more source checks"),
        |_| panic!("no duplicate model"),
    )
    .unwrap();
    let summary = read(&dir.path().join("summary.json")).unwrap();
    assert_eq!(summary["model_results"], 3);
    assert_eq!(summary["setup_failures"], 0);
}

#[test]
fn setup_failure_stops_without_a_score_and_explicit_resume_preserves_receipts() {
    let dir = tempfile::tempdir().unwrap();
    let plan = plan(dir.path(), 2);
    assert!(
        drive(
            &plan,
            &NOT_CANCELLED,
            || Ok(()),
            |entry| {
                receipt(&plan, entry, false);
                Ok(Some(1))
            }
        )
        .is_err()
    );
    let results = read(&dir.path().join("results.json")).unwrap();
    assert_eq!(results[0]["classification"], "setup_failure");
    assert_eq!(results[0]["model_launch_requested"], false);
    assert_eq!(results[0]["score"], Value::Null);
    assert_eq!(results[0]["safe_to_retry_setup"], true);
    let original = fs::read(dir.path().join("run-0-setup-001/acceptance.json")).unwrap();
    let mut order = Vec::new();
    drive(
        &plan,
        &NOT_CANCELLED,
        || Ok(()),
        |entry| {
            order.push((entry.run, entry.setup_attempt));
            receipt(&plan, entry, true);
            Ok(Some(0))
        },
    )
    .unwrap();
    assert_eq!(order, vec![(0, 2), (1, 1)]);
    assert_eq!(
        original,
        fs::read(dir.path().join("run-0-setup-001/acceptance.json")).unwrap()
    );
    assert_eq!(
        read(&dir.path().join("summary.json")).unwrap()["model_launch_intents"],
        2
    );
}

#[test]
fn setup_retries_are_bounded_across_explicit_continuations() {
    let dir = tempfile::tempdir().unwrap();
    let plan = plan(dir.path(), 1);
    let mut count = 0;
    for _ in 0..4 {
        assert!(
            drive(
                &plan,
                &NOT_CANCELLED,
                || Ok(()),
                |entry| {
                    count += 1;
                    receipt(&plan, entry, false);
                    Ok(Some(1))
                }
            )
            .is_err()
        );
    }
    assert_eq!(count, 3);
}

#[test]
fn missing_results_or_failed_cleanup_never_retry_a_model() {
    for failure in [
        "missing-result",
        "cleanup",
        "evidence",
        "manifest",
        "linked-evidence",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let plan = plan(dir.path(), 2);
        assert!(
            drive(
                &plan,
                &NOT_CANCELLED,
                || Ok(()),
                |entry| {
                    receipt(&plan, entry, true);
                    let work = plan.attempt_path(entry);
                    match failure {
                        "missing-result" => fs::remove_file(work.join("benchmark-result.json"))?,
                        "cleanup" => {
                            save(&work.join("acceptance.json"), &json!({"cleanup":"failed"}))?;
                        }
                        "manifest" => {
                            let path = work.join("benchmark-manifest.json");
                            let mut manifest = read(&path)?;
                            manifest["model_requested"] = json!("wrong");
                            save(&path, &manifest)?;
                            let path = work.join("benchmark-result.json");
                            let mut result = read(&path)?;
                            result["evidence_sha256"]["benchmark-manifest.json"] =
                                json!(hash(&work.join("benchmark-manifest.json"))?);
                            save(&path, &result)?;
                        }
                        "linked-evidence" => {
                            fs::rename(
                                work.join("benchmark-context.json"),
                                work.join("context-copy.json"),
                            )?;
                            std::os::unix::fs::symlink(
                                work.join("context-copy.json"),
                                work.join("benchmark-context.json"),
                            )?;
                        }
                        _ => save(&work.join("normalized-calls.json"), &json!(["tampered"]))?,
                    }
                    Ok(Some(1))
                }
            )
            .is_err(),
            "{failure}"
        );
        assert!(
            drive(
                &plan,
                &NOT_CANCELLED,
                || Ok(()),
                |_| panic!("must not relaunch: {failure}")
            )
            .is_err()
        );
    }
}

#[test]
fn a_completed_receipt_survives_driver_interruption_without_relaunch() {
    let dir = tempfile::tempdir().unwrap();
    let plan = plan(dir.path(), 2);
    let entry = Entry {
        run: 0,
        setup_attempt: 1,
        finished: false,
        exit_code: None,
        process_error: None,
        receipts: None,
    };
    save(&dir.path().join("journal.json"), &json!([entry])).unwrap();
    receipt(&plan, &entry, true);
    let mut order = Vec::new();
    drive(
        &plan,
        &NOT_CANCELLED,
        || Ok(()),
        |entry| {
            order.push(entry.run);
            receipt(&plan, entry, true);
            Ok(Some(0))
        },
    )
    .unwrap();
    assert_eq!(order, vec![1]);
}

#[test]
fn absent_receipts_or_partial_cluster_creation_need_manual_inspection() {
    for mode in ["absent", "cluster", "preservation", "uncertain-launch"] {
        let dir = tempfile::tempdir().unwrap();
        let plan = plan(dir.path(), 1);
        assert!(
            drive(
                &plan,
                &NOT_CANCELLED,
                || Ok(()),
                |entry| {
                    if mode != "absent" {
                        receipt(&plan, entry, false);
                        let work = plan.attempt_path(entry);
                        match mode {
                            "cluster" => save(
                                &work.join("state/setup-progress.json"),
                                &json!({"stage":"cluster","status":"failed"}),
                            )?,
                            "preservation" => save(
                                &work.join("preservation-after.json"),
                                &json!({"containers":{"leak":{}}}),
                            )?,
                            _ => fs::write(work.join("model-launch.json"), b"interrupted write")?,
                        }
                    }
                    Ok(None)
                }
            )
            .is_err()
        );
        assert!(
            drive(
                &plan,
                &NOT_CANCELLED,
                || Ok(()),
                |_| panic!("unsafe continuation: {mode}")
            )
            .is_err()
        );
    }
}

#[test]
fn edited_receipts_and_truncated_journals_fail_closed() {
    let dir = tempfile::tempdir().unwrap();
    let plan = plan(dir.path(), 1);
    drive(
        &plan,
        &NOT_CANCELLED,
        || Ok(()),
        |entry| {
            receipt(&plan, entry, true);
            Ok(Some(0))
        },
    )
    .unwrap();
    let path = dir.path().join("run-0-setup-001/benchmark-result.json");
    let mut result = read(&path).unwrap();
    result["accepted_score"] = json!(100);
    save(&path, &result).unwrap();
    assert!(drive(&plan, &NOT_CANCELLED, || Ok(()), |_| panic!("no relaunch")).is_err());
    fs::write(dir.path().join("journal.json"), b"[").unwrap();
    assert!(drive(&plan, &NOT_CANCELLED, || Ok(()), |_| panic!("no launch")).is_err());
}

#[test]
fn cancellation_and_changed_source_stop_before_next_launch() {
    let dir = tempfile::tempdir().unwrap();
    let plan = plan(dir.path(), 2);
    assert!(
        drive(
            &plan,
            &NOT_CANCELLED,
            || anyhow::bail!("changed source"),
            |_| panic!("must not launch")
        )
        .is_err()
    );
    let cancelled = AtomicBool::new(false);
    assert!(
        drive(
            &plan,
            &cancelled,
            || Ok(()),
            |entry| {
                receipt(&plan, entry, true);
                cancelled.store(true, Ordering::SeqCst);
                Ok(Some(0))
            }
        )
        .is_err()
    );
    assert_eq!(
        read(&dir.path().join("summary.json")).unwrap()["model_results"],
        1
    );
    drive(
        &plan,
        &NOT_CANCELLED,
        || Ok(()),
        |entry| {
            assert_eq!(entry.run, 1);
            receipt(&plan, entry, true);
            Ok(Some(0))
        },
    )
    .unwrap();
}

#[test]
fn provider_errors_and_uncertain_launches_are_not_model_scores() {
    for outcome in [
        "provider_or_harness_failure",
        "harness_failure",
        "interrupted",
        "not_started",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let plan = plan(dir.path(), 1);
        assert!(
            drive(
                &plan,
                &NOT_CANCELLED,
                || Ok(()),
                |entry| {
                    receipt(&plan, entry, true);
                    let path = plan.attempt_path(entry).join("benchmark-result.json");
                    let mut result = read(&path)?;
                    result["outcome"] = json!(outcome);
                    save(&path, &result)?;
                    Ok(Some(1))
                }
            )
            .is_err()
        );
        let rows = read(&dir.path().join("results.json")).unwrap();
        assert_eq!(rows[0]["classification"], "model_outcome_unverified");
        assert_eq!(rows[0]["score"], Value::Null);
    }
}

#[test]
fn plan_rejects_changed_contracts_unknown_fields_and_duplicate_slots() {
    let dir = tempfile::tempdir().unwrap();
    let mut plan = plan(dir.path(), 2);
    plan.runs[1].id = plan.runs[0].id.clone();
    assert!(plan.validate().is_err());
    plan.runs.pop();
    plan.runs[0].task_sha256 = "old contract".into();
    assert!(plan.validate().is_err());
    let mut value = json!(plan);
    value["retry_models"] = json!(true);
    assert!(serde_json::from_value::<Plan>(value).is_err());
}

#[test]
fn gate_timeout_cannot_cut_short_the_common_model_allowance() {
    let dir = tempfile::tempdir().unwrap();
    let mut plan = plan(dir.path(), 1);
    assert!(plan.validate().is_ok());
    for timeout in [1200, 1800, 3600, 4199] {
        plan.gate_timeout_seconds = timeout;
        assert!(
            plan.validate()
                .unwrap_err()
                .to_string()
                .contains("model deadline")
        );
    }
    plan.gate_timeout_seconds = 4200;
    assert!(plan.validate().is_ok());
}

#[test]
fn source_validation_checks_runner_and_cli_bytes_before_git_or_launch() {
    let dir = tempfile::tempdir().unwrap();
    let mut plan = plan(dir.path(), 1);
    fs::write(&plan.runner, b"runner").unwrap();
    fs::write(&plan.runs[0].executable, b"CLI").unwrap();
    assert!(
        plan.verify_source()
            .unwrap_err()
            .to_string()
            .contains("runner changed")
    );
    plan.runner_sha256 = hash(&plan.runner).unwrap();
    assert!(
        plan.verify_source()
            .unwrap_err()
            .to_string()
            .contains("CLI changed")
    );
}

#[test]
fn campaign_lock_is_exclusive_and_released_without_deleting_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("lock");
    let open = || {
        fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .unwrap()
    };
    let lock = Flock::lock(open(), FlockArg::LockExclusiveNonblock).unwrap();
    assert!(Flock::lock(open(), FlockArg::LockExclusiveNonblock).is_err());
    drop(lock);
    assert!(Flock::lock(open(), FlockArg::LockExclusiveNonblock).is_ok());
}

#[test]
fn subprocess_uses_exact_cli_arguments_and_a_private_non_reusable_log() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let mut plan = plan(dir.path(), 1);
    fs::write(&plan.runner, b"#!/bin/sh\nprintf '%s\\n' \"$@\"\nexit 0\n").unwrap();
    fs::set_permissions(&plan.runner, fs::Permissions::from_mode(0o700)).unwrap();
    for (index, (harness, option)) in [
        ("codex", "--benchmark-codex"),
        ("claude-code", "--benchmark-claude"),
        ("opencode", "--benchmark-opencode"),
    ]
    .into_iter()
    .enumerate()
    {
        plan.runs[0].harness = harness.into();
        let entry = Entry {
            run: 0,
            setup_attempt: u32::try_from(index + 1).unwrap(),
            finished: false,
            exit_code: None,
            process_error: None,
            receipts: None,
        };
        assert_eq!(execute(&plan, &entry, &NOT_CANCELLED).unwrap(), Some(0));
        let work = plan.attempt_path(&entry);
        let log = work.with_extension("log");
        let args = fs::read_to_string(&log).unwrap();
        let expected = vec![
            "--checkout-home",
            plan.checkout_home.to_str().unwrap(),
            "--root",
            plan.root.to_str().unwrap(),
            "--work-dir",
            work.to_str().unwrap(),
            "--timeout",
            "4200",
            "--benchmark-harness",
            harness,
            option,
            plan.runs[0].executable.to_str().unwrap(),
            "--benchmark-model",
            "model-0",
            "benchmark-o1",
        ];
        assert_eq!(args.lines().collect::<Vec<_>>(), expected);
        assert_eq!(
            fs::metadata(log).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(execute(&plan, &entry, &NOT_CANCELLED).is_err());
    }
}
