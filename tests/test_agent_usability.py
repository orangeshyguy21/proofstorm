"""Evidence grading must not confuse scientific negatives with bad execution."""
import importlib.util
from pathlib import Path
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]


def module(name):
    spec = importlib.util.spec_from_file_location(name, ROOT / 'scripts' / f'{name}.py')
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


evaluator = module('evaluate-agent-usability')
cluster = module('agent-usability-cluster')


class EvaluationTests(unittest.TestCase):
    def setUp(self):
        self.manifest = {'expectations': {}, 'execution_surface': 'native-first', 'manual_gates': []}
        self.metrics = {'workflow': {'raw_exec_calls': 100}, 'exits': {'kubectl': 0}}
        self.score = {'automated_hard_gates': {'baseline': True}}
        self.review = {'reviewer': 'test reviewer', 'gates': {
            name: {'passed': True, 'evidence': ['events.jsonl:1']}
            for name in ('execution_context_appropriate', 'claims_supported_by_observations', 'native_failures_explained')},
            'finding': {'outcome': 'violated', 'evidence': ['events.jsonl:1']}}

    def evaluate(self, **kwargs):
        return evaluator.evaluate(self.manifest, self.metrics, self.score, kwargs.pop('events', []),
                                  kwargs.pop('cluster', {'verified_idle': True}), **kwargs)

    def test_unknown_manual_gates_never_pass(self):
        self.assertEqual(self.evaluate()['proficiency'], 'needs_review')

    def test_native_count_and_target_violation_do_not_fail_execution(self):
        score = self.evaluate(review=self.review)
        self.assertEqual(score['proficiency'], 'passed')
        self.assertEqual(score['assessment']['target_property'], 'violated')

    def test_missing_cleanup_observation_fails_closed(self):
        self.assertEqual(self.evaluate(cluster={})['proficiency'], 'failed')

    def test_operator_cleanup_cannot_rescue_agent_pass(self):
        self.assertEqual(self.evaluate(review=self.review, operator_cleanup=[{'name': 'lab'}])['proficiency'], 'failed')

    def test_typed_only_contract_still_restricts_exec(self):
        self.manifest['execution_surface'] = 'typed-contract'
        self.assertEqual(self.evaluate(review=self.review)['proficiency'], 'failed')

    def test_internal_todos_are_not_host_execution(self):
        event = {'type': 'tool_use', 'part': {'tool': 'todowrite', 'state': {'status': 'completed'}}}
        self.assertEqual(self.evaluate(events=[event], review=self.review)['proficiency'], 'passed')

    def test_host_shell_is_not_native_component_execution(self):
        event = {'type': 'tool_use', 'part': {'tool': 'bash', 'state': {'status': 'completed'}}}
        self.assertEqual(self.evaluate(events=[event], review=self.review)['proficiency'], 'failed')

    def test_review_requires_evidence(self):
        self.review['gates']['execution_context_appropriate']['evidence'] = []
        self.assertEqual(self.evaluate(review=self.review)['proficiency'], 'needs_review')

    def test_good_execution_does_not_validate_an_unsupported_conclusion(self):
        self.review['gates']['claims_supported_by_observations']['passed'] = False
        score = self.evaluate(review=self.review)
        self.assertEqual(score['assessment']['execution'], 'valid')
        self.assertEqual(score['assessment']['evidence'], 'insufficient')
        self.assertEqual(score['proficiency'], 'failed')

    def test_unpaid_without_reservation_is_not_release(self):
        self.manifest['expectations']['reservation_release_required'] = True
        self.assertFalse(self.evaluate()['automated_hard_gates']['actual_reservation_release_observed'])

    def test_inconclusive_reservation_does_not_mask_false_claim(self):
        self.manifest['expectations']['reservation_release_required'] = True
        self.review['gates']['claims_supported_by_observations']['passed'] = False
        score = self.evaluate(review=self.review)
        self.assertEqual(score['assessment']['execution'], 'inconclusive')
        self.assertEqual(score['assessment']['evidence'], 'insufficient')
        self.assertEqual(score['proficiency'], 'failed')

    def test_release_requires_restored_available_value(self):
        import json
        self.manifest['expectations']['reservation_release_required'] = True
        content = {'state_after': 'UNPAID', 'melt_quote_id': 'quote',
                   'reserved_proof_count_before': 2, 'reserved_proof_count_after': 0,
                   'reserved_sat_before': 64, 'reserved_sat_after': 0,
                   'available_balance_sat_before': 100, 'available_balance_sat_after': 164}

        def event():
            op = {'operation_id': 'refresh', 'kind': 'wallet_melt_quote_refresh',
                  'phase': 'succeeded', 'artifact': {'content': content}}
            return {'type': 'tool_use', 'part': {'tool': 'proofstorm_proofstorm_operation_wait_many',
                    'state': {'status': 'completed', 'output': json.dumps({'operations': [op]})}}}

        self.assertTrue(self.evaluate(events=[event()])['automated_hard_gates']['actual_reservation_release_observed'])
        content['available_balance_sat_after'] = 163
        self.assertFalse(self.evaluate(events=[event()])['automated_hard_gates']['actual_reservation_release_observed'])

    def test_repeated_observation_is_one_native_operation(self):
        import json
        op = {'operation_id': 'one', 'kind': 'component_exec_live', 'phase': 'succeeded',
              'artifact': {'content': {'exit_code': 1, 'execution_context': 'live_component'}}}
        event = {'type': 'tool_use', 'part': {'tool': 'proofstorm_proofstorm_operation_wait_many',
                 'state': {'status': 'completed', 'output': json.dumps({'operations': [op]})}}}
        score = self.evaluate(events=[event, event], review=self.review)
        self.assertEqual(len(score['execution_surface']['native_operations']), 1)
        self.assertEqual(score['execution_surface']['counts']['native_nonzero_operations'], 1)
        self.assertEqual(score['proficiency'], 'passed')

    def test_cluster_counts_orphaned_storage_and_candidate_resources(self):
        items = [
            {'kind': 'Pod', 'metadata': {'name': 'controller', 'namespace': 'proofstorm-system',
             'labels': {'app.kubernetes.io/name': 'proofstormd'}}},
            {'kind': 'PersistentVolume', 'metadata': {'name': 'orphan'},
             'spec': {'claimRef': {'namespace': 'proofstorm-i123'}}},
            {'kind': 'ProofstormCandidateBuild', 'metadata': {'name': 'candidate', 'namespace': 'proofstorm-system'}},
        ]
        with patch.object(cluster, 'resources', return_value=items):
            state = cluster.snapshot()
        self.assertFalse(state['verified_idle'])
        self.assertEqual(len(state['blockers']), 2)

    def test_bound_controller_custody_is_infrastructure_but_unmounted_claim_blocks(self):
        items = [
            {'kind': 'Pod', 'metadata': {'name': 'controller', 'namespace': 'proofstorm-system',
             'labels': {'app.kubernetes.io/name': 'proofstormd'}},
             'spec': {'volumes': [{'persistentVolumeClaim': {'claimName': 'proofstormd-private'}}]}},
            {'kind': 'PersistentVolumeClaim', 'metadata': {'name': 'proofstormd-private',
             'namespace': 'proofstorm-system'}, 'status': {'phase': 'Bound'}},
        ]
        with patch.object(cluster, 'resources', return_value=items):
            self.assertTrue(cluster.snapshot()['verified_idle'])
        items.append({'kind': 'PersistentVolumeClaim', 'metadata': {'name': 'unowned',
                      'namespace': 'proofstorm-system'}, 'status': {'phase': 'Bound'}})
        with patch.object(cluster, 'resources', return_value=items):
            state = cluster.snapshot()
        self.assertEqual([x['name'] for x in state['blockers']], ['unowned'])


if __name__ == '__main__':
    unittest.main()
