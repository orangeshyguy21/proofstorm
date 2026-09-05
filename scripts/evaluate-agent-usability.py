#!/usr/bin/env python3
"""Add evidence-linked, native-first assessment to a benchmark scorecard.

Review format: {reviewer, gates: {name: {passed: bool, evidence: [references]}},
finding: {outcome: held|violated|inconclusive|not_applicable, evidence: [...]}}.
The reviewer reads the transcript and artifacts; heuristics never decide intent.
"""
import argparse
import hashlib
import json
from pathlib import Path


def read(path, default=None):
    return json.loads(path.read_text()) if path.exists() else default


def observations(events):
    calls, operations = [], {}
    for line, event in enumerate(events, 1):
        if event.get('type') != 'tool_use':
            continue
        part = event.get('part', {})
        state = part.get('state', {})
        calls.append({'line': line, 'tool': part.get('tool', ''), **state})
        try:
            output = json.loads(state.get('output', '{}'))
        except (ValueError, TypeError):
            continue
        if not isinstance(output, dict):
            continue
        for op in [output, *output.get('operations', [])]:
            identity = op.get('operation_id') or op.get('id')
            if identity and op.get('artifact', {}).get('content') is not None:
                operations[identity] = {**op, 'event_line': line}
    return calls, operations


def evaluate(manifest, metrics, score, events, cluster, review=None, operator_cleanup=None):
    expected = manifest['expectations']
    surface = manifest.get('execution_surface', 'typed-contract' if expected.get('raw_exec_max') == 0 else 'mixed')
    calls, operations = observations(events)
    native = []
    releases = []
    for identity, op in operations.items():
        content = op['artifact']['content']
        if op.get('kind') in ('component_exec_live', 'component_forensics'):
            native.append({'operation_id': identity, 'kind': op['kind'],
                           'component': content.get('component'), 'phase': op.get('phase'),
                           'execution_context': content.get('execution_context'),
                           'exit_code': content.get('exit_code'),
                           'truncated': content.get('output_truncated'),
                           'timed_out': content.get('timed_out'),
                           'artifact_digest': op['artifact'].get('digest'),
                           'evidence': f'events.jsonl:{op["event_line"]}'})
        if op.get('kind') == 'wallet_melt_quote_refresh':
            before = content.get('reserved_sat_before', 0)
            after = content.get('reserved_sat_after')
            available_before = content.get('available_balance_sat_before')
            available_after = content.get('available_balance_sat_after')
            if (op.get('phase') == 'succeeded' and content.get('state_after') == 'UNPAID'
                    and content.get('reserved_proof_count_before', 0) > 0
                    and content.get('reserved_proof_count_after') == 0
                    and before > 0 and after == 0
                    and isinstance(available_before, int) and isinstance(available_after, int)
                    and available_after - available_before == before):
                releases.append({'operation_id': identity, 'quote_id': content.get('melt_quote_id'),
                                 'reserved_sat_released': before,
                                 'evidence': f'events.jsonl:{op["event_line"]}'})
    score['schema_version'] = 2
    timestamps = [event['timestamp'] for event in events if isinstance(event.get('timestamp'), (int, float))]
    score['agent_wall_time_seconds'] = round((max(timestamps) - min(timestamps)) / 1000, 3) if timestamps else None
    gates = score['automated_hard_gates']
    gates['cluster_verified_idle'] = cluster.get('verified_idle') is True
    gates['cluster_query_succeeded'] = metrics.get('exits', {}).get('kubectl') == 0
    gates['within_run_limits'] = metrics.get('limit_reason') is None
    gates['no_operator_cleanup_required'] = not operator_cleanup
    gates['candidate_build_upper_bound_met'] = metrics.get('candidate', {}).get('builds', 0) <= expected.get('candidate_builds_max', float('inf'))
    gates['execution_surface_contract_met'] = (
        surface != 'typed-contract' or metrics['workflow']['raw_exec_calls'] <= expected.get('raw_exec_max', 0))
    gates['native_execution_observed'] = sum(
        op['kind'] == 'component_exec_live' and op['phase'] == 'succeeded'
        and op['exit_code'] == 0 and op['execution_context'] == 'live_component'
        for op in native) >= expected.get('live_exec_successes_min', 0)
    gates['actual_reservation_release_observed'] = not expected.get('reservation_release_required', False) or bool(releases)
    gates['no_host_tools_used'] = all(call['tool'].startswith('proofstorm_') or
                                     call['tool'] in ('todowrite', 'todoread') for call in calls)

    required = list(manifest.get('manual_gates', []))
    for name in ('execution_context_appropriate', 'claims_supported_by_observations', 'native_failures_explained'):
        if name not in required:
            required.append(name)
    review = review or {}
    manual = {}
    for name in required:
        decision = review.get('gates', {}).get(name, {})
        valid = (bool(review.get('reviewer')) and type(decision.get('passed')) is bool
                 and isinstance(decision.get('evidence'), list) and bool(decision['evidence'])
                 and all(isinstance(ref, str) and ref.strip() for ref in decision['evidence']))
        manual[name] = decision['passed'] if valid else None
    score['manual_hard_gates'] = manual
    score['review'] = review
    score['evaluator_digest'] = 'sha256:' + hashlib.sha256(Path(__file__).read_bytes()).hexdigest()
    auto_pass = all(value is True for value in gates.values())
    manual_fail = any(value is False for value in manual.values())
    manual_complete = all(value is True for value in manual.values())
    evidence_gates = {
        'claims_supported_by_observations', 'native_failures_explained',
        'typed_surface_choices_justified', 'conclusions_supported',
        'fee_claims_internally_consistent', 'ordered_evidence_supports_claims',
        'reservation_claim_supported', 'settlement_claim_supported',
        'security_conclusion_supported', 'connectivity_claim_supported',
    }
    execution_review = [value for name, value in manual.items() if name not in evidence_gates]
    evidence_review = [value for name, value in manual.items() if name in evidence_gates]
    finding = review.get('finding', {})
    finding_valid = (bool(review.get('reviewer')) and finding.get('outcome') in
                     ('held', 'violated', 'inconclusive', 'not_applicable')
                     and isinstance(finding.get('evidence'), list) and bool(finding['evidence'])
                     and all(isinstance(ref, str) and ref.strip() for ref in finding['evidence']))
    missing_reservation_only = (not gates['actual_reservation_release_observed'] and
                                all(value is True for name, value in gates.items()
                                    if name != 'actual_reservation_release_observed'))
    score['assessment'] = {
        'execution': 'failed' if False in execution_review else 'inconclusive' if missing_reservation_only else 'failed' if not auto_pass else 'valid' if all(value is True for value in execution_review) else 'needs_review',
        'target_property': finding['outcome'] if finding_valid else 'unreviewed',
        'evidence': 'insufficient' if False in evidence_review else 'sufficient' if all(value is True for value in evidence_review) and finding_valid else 'needs_review',
    }
    score['proficiency'] = ('failed' if manual_fail else 'inconclusive' if missing_reservation_only else 'failed' if not auto_pass else
                            'passed' if manual_complete and finding_valid and finding['outcome'] != 'inconclusive'
                            else 'inconclusive' if manual_complete and finding_valid else 'needs_review')
    diagnostics = score.pop('thrash', score.get('diagnostics', {}))
    for descriptive_count in ('raw_exec_calls', 'native_exec_nonzero_exits', 'recoverable_error_budget'):
        diagnostics.pop(descriptive_count, None)
    score['diagnostics'] = diagnostics
    score['execution_surface'] = {
        'policy': surface, 'native_operations': native, 'reservation_releases': releases,
        'counts': {
            'live_exec_calls': metrics['workflow'].get('live_exec_calls', 0),
            'forensics_calls': metrics['workflow'].get('forensics_calls', 0),
            'native_nonzero_operations': sum(op['exit_code'] is not None and op['exit_code'] != 0 for op in native),
        },
        'native_nonzero_exits_are_observations': True,
        'diagnostic_calls': [{'tool': c['tool'], 'evidence': f'events.jsonl:{c["line"]}',
                              'status': c.get('status')}
                             for c in calls if c.get('status') == 'error'],
        'review_dimensions': ['wrong_context', 'wrapper_hunting', 'credential_or_socket_plumbing',
                              'repeated_ineffective_commands', 'unverified_mutations', 'typed_wrapper_benefit'],
        'reviewed_findings': review.get('surface_findings', []),
    }
    return score


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('run', type=Path)
    parser.add_argument('--review', type=Path)
    args = parser.parse_args()
    events = [json.loads(line) for line in (args.run / 'events.jsonl').read_text().splitlines() if line.strip()]
    score = evaluate(read(args.run / 'manifest.json'), read(args.run / 'metrics.json'),
                     read(args.run / 'scorecard.json'), events,
                     read(args.run / 'cluster-after.json', {}),
                     read(args.review) if args.review else read(args.run / 'review.json'),
                     read(args.run / 'operator-cleanup.json', []))
    (args.run / 'scorecard.json').write_text(json.dumps(score, indent=2) + '\n')
    print(json.dumps({'run_id': score['run_id'], 'assessment': score['assessment'],
                      'proficiency': score['proficiency']}))


if __name__ == '__main__':
    main()
