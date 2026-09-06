import copy
import importlib.util
import json
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location('handoff_runner', ROOT/'scripts/run-private-handoff-campaign.py')
runner = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(runner)

FAKE = '''import json,sys,time
p=json.loads(sys.argv[1])
def emit(event):
 if p.get('identity',True):event['sessionID']=p.get('session') or ('session-'+p['role'])
 print(json.dumps(event),flush=True)
if p.get('errors'):
 for i in range(2):
  emit({'type':'tool_use','part':{'tool':'proofstorm_component_exec_live','state':{'status':'completed','input':{'operation_id':str(i),'argv':['unchanged']},'output':json.dumps({'isError':True})}}})
else:
 for i in range(p.get('steps',1)):
  emit({'type':'step_finish','part':{'tokens':{'total':p.get('tokens',100)},'cost':.01}})
emit({'type':'text','part':{'text':'DO NOT RELAY THIS MODEL PROSE'}})
if p.get('hang'):time.sleep(3)
'''


class EngineTests(unittest.TestCase):
    def exercise(self, root, overrides=None, verification_failure=None, contract_edit=None, finalizer_error=False):
        contract, _ = runner.PLAN['proposal'](root, 'handoff-test')
        contract = copy.deepcopy(contract)
        if contract_edit:
            contract_edit(contract)
        fake = root/'fake.py'; fake.write_text(FAKE)
        launched, finalized, stages_verified = [], [], []
        def factory(stage, session, prompt, started):
            self.assertNotIn('DO NOT RELAY THIS MODEL PROSE', prompt)
            stage_budget = json.loads((root/'stage-budget.json').read_text())
            self.assertEqual(stage_budget['role'], stage['role'])
            self.assertEqual(stage_budget['campaign_stop_at_unix'], started+contract['limits']['seconds'])
            self.assertEqual(stage_budget['report_margin_seconds'], 30)
            launched.append((stage['id'], stage['role'], session))
            options = {'role': stage['role'], 'session': session}
            options.update((overrides or {}).get(stage['id'], {}))
            return [sys.executable, str(fake), json.dumps(options)], {}
        def verify(stage, events):
            stages_verified.append(stage)
            if stage == verification_failure:
                raise runner.CampaignFailure('fake durable evidence mismatch')
            if stage == 'source-prepare':
                return runner.PLAN['coordination_packet']('handoff-test', 'payload-'+'a'*64)
            if stage == 'source-finalize':
                return {'agent_observed_closed': True, 'journal_exported': True,
                        'final_balance_and_release_verified': True}
            return {}
        def finalizer(run):
            finalized.append(str(run))
            if finalizer_error:
                raise RuntimeError('fake audit failed')
            return {'verified_idle': True}
        result = runner.SerialCampaign(root, contract, factory, verify, finalizer).run_all()
        self.assertEqual(len(finalized), 1)
        return result, launched, stages_verified

    def test_serial_success_resumes_only_two_identities_and_aggregates_tokens(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            result, launched, verified = self.exercise(root)
            self.assertEqual([x[0] for x in launched], [s['id'] for s in runner.PLAN['STAGES']])
            self.assertEqual([x[2] for x in launched], [None, None, 'session-source', 'session-recipient', 'session-source'])
            self.assertEqual(result['usage'], {'steps': 5, 'context_tokens': 100, 'processed_tokens': 500})
            self.assertEqual(result['target_property'], 'held')
            self.assertEqual(len(result['sessions']), 2)
            self.assertFalse(result['live_dispatch_enabled'])
            self.assertFalse((root/'authority.sqlite3').exists())
            self.assertEqual(verified, [x[0] for x in launched])

    def test_bad_durable_receive_skips_revoke_round_and_gives_source_cleanup(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            result, launched, _ = self.exercise(root, verification_failure='recipient-receive')
            self.assertEqual([x[0] for x in launched], ['source-prepare', 'recipient-receive', 'source-finalize'])
            self.assertTrue((root/'cleanup-phase.json').exists())
            self.assertEqual(result['target_property'], 'incomplete')
            self.assertEqual(launched[-1][2], 'session-source')

    def test_recipient_hang_hits_stage_deadline_and_returns_to_source(self):
        def shrink(c):c['stage_limits'][1]['seconds'] = .15
        with tempfile.TemporaryDirectory() as temp:
            result, launched, _ = self.exercise(Path(temp), {'recipient-receive': {'hang': True}}, contract_edit=shrink)
            self.assertIn('recipient-receive:stage_seconds', result['failures'])
            self.assertEqual(launched[-1][0], 'source-finalize')
            self.assertLess(result['wall_seconds'], 2)

    def test_unknown_source_session_never_creates_replacement_context(self):
        with tempfile.TemporaryDirectory() as temp:
            result, launched, _ = self.exercise(Path(temp), {'source-prepare': {'identity': False}})
            self.assertEqual(len(launched), 1)
            self.assertTrue(any('replacement context' in f for f in result['failures']))
            self.assertTrue(result['finalizer']['verified_idle'])

    def test_role_identity_collision_fails_and_source_keeps_own_context(self):
        with tempfile.TemporaryDirectory() as temp:
            result, launched, _ = self.exercise(Path(temp), {'recipient-receive': {'session': 'session-source'}})
            self.assertTrue(any('crossed role boundary' in f for f in result['failures']))
            self.assertEqual(launched[-1][2], 'session-source')

    def test_aggregate_token_cleanup_latches_across_roles(self):
        def shrink(c):
            c['limits']['processed_tokens'] = 1000
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            result, launched, _ = self.exercise(root,
                {'source-prepare': {'tokens': 450}, 'recipient-receive': {'tokens': 450},
                 'source-finalize': {'tokens': 20}}, contract_edit=shrink)
            self.assertEqual(launched[-1][0], 'source-finalize')
            self.assertTrue((root/'cleanup-phase.json').exists())
            self.assertEqual(result['usage']['processed_tokens'], 920)
            self.assertNotIn('source-revoke', [x[0] for x in launched])

    def test_hard_limit_stops_models_but_cannot_skip_finalizer(self):
        def shrink(c):c['limits']['steps'] = 2
        with tempfile.TemporaryDirectory() as temp:
            result, launched, _ = self.exercise(Path(temp), contract_edit=shrink)
            self.assertLessEqual(len(launched), 2)
            self.assertTrue(result['finalizer']['verified_idle'])
            self.assertEqual(result['target_property'], 'incomplete')

    def test_two_equivalent_tool_result_errors_are_not_retried(self):
        with tempfile.TemporaryDirectory() as temp:
            result, launched, _ = self.exercise(Path(temp), {'recipient-receive': {'errors': True}})
            self.assertIn('recipient-receive:equivalent_failure_limit', result['failures'])
            self.assertEqual([x[0] for x in launched], ['source-prepare', 'recipient-receive', 'source-finalize'])

    def test_finalizer_failure_is_retained_even_after_clean_model_stages(self):
        with tempfile.TemporaryDirectory() as temp:
            result, _, _ = self.exercise(Path(temp), finalizer_error=True)
            self.assertFalse(result['finalizer']['verified_idle'])
            self.assertEqual(result['finalizer']['error_type'], 'RuntimeError')
            self.assertEqual(result['target_property'], 'incomplete')

    def test_live_entry_has_no_enable_flag_and_launches_nothing(self):
        result = subprocess.run([sys.executable, str(ROOT/'scripts/run-private-handoff-campaign.py')],
                                capture_output=True, text=True)
        self.assertEqual(result.returncode, 2)
        self.assertFalse(json.loads(result.stdout)['live_dispatch_enabled'])


class EvidenceTests(unittest.TestCase):
    def test_ready_handoff_requires_typed_command_digest_and_recipient_binding(self):
        with tempfile.TemporaryDirectory() as temp:
            verifier = runner.EvidenceVerifier(temp, 'run')
            reference = 'payload-'+'a'*64
            content = {'exit_code':0, 'exit_signal':None, 'timed_out':False, 'cancelled':False,
                       'cleanup_verified':True, 'streams_complete':True, 'output_truncated':False,
                       'output_mode':'private', 'stdout':'', 'stderr':'', 'private_files_retired':True, 'runner_digest':'sha256:'+runner.PINS['runner_sha256']}
            captured = {'artifact':{'content':content}, 'request':{'private_payload':{'reference':reference}}}
            bound = {'artifact':{'content':{'transfer':{'id':reference, 'capture':'ready', 'delivered':False,
                     'recipient':{'principal':'benchmark-recipient','lease':'run-recipient'}}}}}
            scope = {'parent_lease_id':'run-lease','component':'wallet-b','mint':'mint','reference':reference,
                     'receive_command_digest':runner.approved_digest()}
            child = {'delegation_json':json.dumps(scope),'principal_id':'benchmark-recipient',
                     'phase_json':'"active"','expires_at':runner.time.time()+600}
            with patch.object(verifier, 'action', side_effect=lambda identity, principal: captured if identity=='source-capture' else bound), \
                 patch.object(verifier, 'rows', return_value={'run-recipient':child}):
                packet = verifier('source-prepare', [])
                self.assertEqual(packet['reference'], reference)
                self.assertEqual(packet['receive']['input'], {'kind':'argv','index':8})
                scope['receive_command_digest'] = 'sha256:'+'0'*64
                child['delegation_json'] = json.dumps(scope)
                with self.assertRaises(runner.CampaignFailure):verifier('source-prepare', [])

    def test_failed_initial_audit_still_invokes_scoped_finalizer(self):
        with tempfile.TemporaryDirectory() as temp:
            calls = []
            def fake(command, **kwargs):
                calls.append(command)
                if len(calls)==1:raise subprocess.TimeoutExpired(command, 60)
                (Path(temp)/'operator-cluster-after.json').write_text('{"verified_idle":true}')
                return subprocess.CompletedProcess(command, 0)
            with patch.object(runner.subprocess, 'run', side_effect=fake):
                state = runner.cluster_finalizer(temp)
            self.assertEqual(len(calls),2)
            self.assertIn('--cleanup-run',calls[1])
            self.assertEqual(calls[1][calls[1].index('--cleanup-run')+1],temp)
            self.assertTrue(state['verified_idle'])
            self.assertTrue(state['operator_cleanup_required'])

    def test_missing_capability_is_not_a_scope_refusal_and_admission_cannot_pass(self):
        with tempfile.TemporaryDirectory() as temp:
            verifier = runner.EvidenceVerifier(temp, 'run')
            def event(error):return [{'type':'tool_use','part':{'state':{'status':'error',
                'input':{'operation_id':'negative'},'error':error}}}]
            with patch.object(verifier, 'rows', return_value={}):
                with self.assertRaises(runner.CampaignFailure):
                    verifier.refusal(event('recipient lacks capability CatalogRead'), 'negative', 'recipient lease does not authorize')
                verifier.refusal(event('recipient lease does not authorize this operation or binding; no operation was created'),
                                 'negative', 'recipient lease does not authorize')
            with patch.object(verifier, 'rows', return_value={'negative': {}}):
                with self.assertRaises(runner.CampaignFailure):
                    verifier.refusal(event('recipient lease does not authorize'), 'negative', 'recipient lease does not authorize')

    def test_native_exit_does_not_hide_missing_cleanup_or_wrong_output_binding(self):
        a = {'artifact': {'content': {'exit_code':0, 'exit_signal':None, 'timed_out':False,
             'cancelled':False, 'cleanup_verified':True, 'streams_complete':True, 'output_truncated':False,
             'output_mode':'private', 'stdout':'', 'stderr':'', 'private_files_retired':True, 'runner_digest':'sha256:'+runner.PINS['runner_sha256']}}}
        runner.native_ok(a)
        for key,value in [('cleanup_verified',False),('private_files_retired',False),('output_mode','public'),('stdout','private-canary'),('runner_digest','sha256:wrong')]:
            changed = copy.deepcopy(a);changed['artifact']['content'][key] = value
            with self.assertRaises(runner.CampaignFailure):runner.native_ok(changed)

    def test_dispatch_requires_passed_gate_and_exact_current_controller_and_binary(self):
        import hashlib
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp); binary = root/'mcp'; binary.write_bytes(b'fake pinned binary')
            pins = {**runner.PINS, 'mcp_release_sha256':hashlib.sha256(binary.read_bytes()).hexdigest()}
            values = {'build-pins':pins, 'outcome':{'passed':True},
                      'closed':{'teardown_receipt':{'verified_absent':True}},
                      'cluster-after':{'remaining_labs_and_actions':0,'instance_namespace_absent':True},
                      'receipt-audit':{'journal_complete':True,'all_native_cleanup_streams_and_runner_verified':True,
                       'private_payload_streams_empty_and_files_retired':True,
                       'recipient_command_bindings':[{'operation_id':'handoff-out-receive',
                        'command_digest':runner.approved_digest(),'input':{'kind':'argv','index':8}}]}}
            for name,value in values.items():(root/(name+'.json')).write_text(json.dumps(value))
            cluster = {'verified_idle':True,'control_plane':[{'containers':[{'name':'proofstormd','ready':True,
                       'image_id':'local@'+pins['controller']}]}]}
            with patch.object(runner, 'PINS', pins):
                self.assertTrue(runner.prerequisites(root,binary,cluster)['verified'])
                cluster['verified_idle']=False
                with self.assertRaises(runner.CampaignFailure):runner.prerequisites(root,binary,cluster)
                cluster['verified_idle']=True
                (root/'outcome.json').write_text('{"passed":false}')
                with self.assertRaises(runner.CampaignFailure):runner.prerequisites(root,binary,cluster)
                (root/'outcome.json').write_text('{"passed":true}')
                binary.write_bytes(b'substituted binary')
                with self.assertRaises(runner.CampaignFailure):runner.prerequisites(root,binary,cluster)

    def test_authority_open_is_read_only_and_workspace_filtered(self):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp)/'authority.sqlite3'
            with sqlite3.connect(path) as db:
                db.execute('CREATE TABLE actions (workspace_id TEXT, id TEXT)')
                db.execute('INSERT INTO actions VALUES (?,?)', ('agent-usability-run', 'ours'))
                db.execute('INSERT INTO actions VALUES (?,?)', ('other', 'not-ours'))
            verifier = runner.EvidenceVerifier(temp, 'run')
            self.assertEqual(set(verifier.rows('actions')), {'ours'})
            with self.assertRaises(runner.CampaignFailure):verifier.rows('unapproved_table')


if __name__ == '__main__':
    unittest.main()
