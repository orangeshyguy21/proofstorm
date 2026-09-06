import importlib.util
import copy
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location('proxy', ROOT / 'scripts/native-execution-proxy.py')
proxy = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(proxy)


class CleanupAdmissionTests(unittest.TestCase):
    def test_campaign_stage_margin_is_visible_on_success_and_refusal(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            stage = root/'stage.json'
            stage.write_text(json.dumps({'id':'recipient-receive','role':'recipient','stop_at_unix':1100}))
            gate = proxy.CleanupGate(root/'events', root/'state', 1000, 600, 50, stage_budget=stage)
            with patch.object(proxy.time, 'time', return_value=1080):
                reply = gate.decorate({'result':{'structuredContent':{'phase':'succeeded'}}})
                b = reply['result']['structuredContent']['_benchmark_budget']
                self.assertEqual(b['seconds_to_stage_stop'],20)
                self.assertEqual(b['hard_stop_at_unix'],1600)
                self.assertEqual(b['report_margin_seconds'],30)
                refused = gate.decorate({'error':{'code':-32600,'message':'lease_owner_mismatch'}})
                self.assertIn('1600',refused['error']['message'])
                self.assertIn('recipient-receive',refused['error']['message'])
                self.assertIn('lease_owner_mismatch',refused['error']['message'])
                bounded = gate.bound_wait({'method':'tools/call','params':{'name':'proofstorm_operation_wait_many',
                    'arguments':{'timeout_seconds':60}}})
                self.assertEqual(bounded['params']['arguments']['timeout_seconds'],1)
                stage.unlink()
                self.assertTrue(gate.decorate({'error':{'message':'original refusal'}})['error']['data']['_benchmark_budget']['budget_unavailable'])

    def test_cleanup_custody_methods_forward_only_status_and_release(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            server = root / 'server.py'
            server.write_text('import sys,json\nfor line in sys.stdin:\n m=json.loads(line);print(json.dumps({"jsonrpc":"2.0","id":m["id"],"result":{"forwarded":True}}),flush=True)\n')
            methods = ['status', 'release', 'prepare', 'deliver', 'handoff', None, ['release']]
            requests = [{'jsonrpc': '2.0', 'id': i, 'method': 'tools/call', 'params': {
                'name': 'proofstorm_private_transfer', 'arguments': {'transfer': {'transferMethod': method}}}}
                for i, method in enumerate(methods)]
            result = subprocess.run([sys.executable, str(ROOT/'scripts/native-execution-proxy.py'),
                '--events', str(root/'events'), '--state', str(root/'state'), '--started-at', '0',
                '--max-seconds', '600', '--max-steps', '50', '--', sys.executable, str(server)],
                input=''.join(json.dumps(x)+'\n' for x in requests), text=True,
                capture_output=True, timeout=10, check=True)
            replies = {r['id']: r for r in map(json.loads, result.stdout.splitlines())}
            for i in [0, 1]:
                self.assertTrue(replies[i]['result']['forwarded'])
            for i in range(2, len(methods)):
                self.assertEqual(replies[i]['error']['data']['code'], 'cleanup_phase_only')

    def test_scoped_public_guard_allows_exact_help_and_private_only(self):
        def request(args):
            return {'method': 'tools/call', 'params': {'name': 'proofstorm_component_exec_live', 'arguments': args}}
        for argv in [['cocod', 'send', 'cashu', '--help'], list(proxy.CDK_PREFIX) + ['send', '--help']]:
            self.assertTrue(proxy.public_output_allowed(request({'argv': argv, 'output': {'mode': 'public'}})))
        self.assertTrue(proxy.public_output_allowed(request({'script': 'arbitrary native operation', 'output': {'mode': 'private'}})))
        for args in [
            {'argv': list(proxy.CDK_PREFIX) + ['check-pending']},
            {'script': 'echo secret --help'},
            {'argv': ['python3', '-c', 'print("secret")', '--help']},
        ]:
            self.assertFalse(proxy.public_output_allowed(request({**args, 'output': {'mode': 'public'}})))

    def test_public_guard_refuses_before_child_and_logs_no_command_body(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            server = root/'server.py'
            server.write_text('import sys,json\nfor line in sys.stdin:\n m=json.loads(line);print(json.dumps({"jsonrpc":"2.0","id":m["id"],"result":{"forwarded":True}}),flush=True)\n')
            requests = [{'jsonrpc':'2.0','id':i,'method':'tools/call','params':{'name':'proofstorm_component_exec_live','arguments':args}}
                        for i,args in enumerate([
                            {'argv':list(proxy.CDK_PREFIX)+['check-pending'],'output':{'mode':'public'}},
                            {'script':'secret-test-canary','output':{'mode':'public'}},
                            {'argv':['cocod','--help'],'output':{'mode':'public'}},
                            {'script':'native-private','output':{'mode':'private'}},
                        ],1)]
            result = subprocess.run([sys.executable,str(ROOT/'scripts/native-execution-proxy.py'),
                '--events',str(root/'events'),'--state',str(root/'state'),'--started-at',str(proxy.time.time()),
                '--max-seconds','600','--max-steps','50','--public-help-only','--argument-audit',str(root/'audit'),
                '--',sys.executable,str(server)],input=''.join(json.dumps(x)+'\n' for x in requests),text=True,capture_output=True,timeout=10)
            self.assertEqual(result.returncode,0,result.stderr)
            replies={r['id']:r for r in map(json.loads,result.stdout.splitlines())}
            for i in [1,2]:self.assertEqual(replies[i]['error']['data']['code'],'public_output_help_only')
            for i in [3,4]:self.assertTrue(replies[i]['result']['forwarded'])
            self.assertNotIn('secret-test-canary',(root/'audit').read_text())

    def test_argument_audit_records_only_known_custody_metadata(self):
        args = {'operation_id': 'agent-prepare', 'transfer': {'transferMethod': 'prepare',
                'component': 'wallet-a', 'destinationComponent': 'wallet-b', 'maximumBytes': 65536}}
        message = {'method': 'tools/call', 'params': {'name': 'proofstorm_private_transfer', 'arguments': args}}
        record = proxy.argument_snapshot(message, 'test')
        self.assertEqual(record['fields']['destinationComponent'], {'state': 'allowed', 'value': 'wallet-b'})
        self.assertEqual(record['fields']['reference'], {'state': 'missing'})
        args['transfer'].update(component='cashuAsecretcanary', reference='credential-secret-canary',
                                extra='never-record-this')
        args['script'] = 'preimage-secret-canary'
        text = json.dumps(proxy.argument_snapshot(message, 'test'))
        for secret in ['cashuAsecretcanary', 'credential-secret-canary', 'never-record-this', 'preimage-secret-canary']:
            self.assertNotIn(secret, text)
        message['params']['name'] = 'proofstorm_component_exec_live'
        self.assertIsNone(proxy.argument_snapshot(message, 'test'))

    def test_boundary_latches_and_permits_only_cleanup_tools(self):
        with tempfile.TemporaryDirectory() as temp:
            events, state = Path(temp)/'events', Path(temp)/'state'
            gate = proxy.CleanupGate(events, state, 1000, 900, 60)
            self.assertFalse(gate.cleanup(now=1719))
            events.write_text('\n'.join(json.dumps({'type': 'step_finish'}) for _ in range(48)))
            self.assertTrue(gate.cleanup(now=1719))
            events.write_text('')
            self.assertTrue(gate.cleanup(now=1000))
            for tool in ['proofstorm_component_exec_live', 'proofstorm_component_forensics', 'proofstorm_lab_apply', 'proofstorm_wallet_fund', 'unknown_tool']:
                self.assertFalse(gate.allows({'method': 'tools/call', 'params': {'name': tool}}))
            for tool in ['proofstorm_action_cancel', 'proofstorm_operation_wait', 'proofstorm_artifact_export', 'proofstorm_lab_close']:
                self.assertTrue(gate.allows({'method': 'tools/call', 'params': {'name': tool}}))
            self.assertTrue(gate.allows({'method': 'initialize'}))

    def test_time_boundary_without_completed_steps(self):
        with tempfile.TemporaryDirectory() as temp:
            gate = proxy.CleanupGate(Path(temp)/'events', Path(temp)/'state', 1000, 900, 60)
            self.assertFalse(gate.cleanup(now=1719))
            self.assertTrue(gate.cleanup(now=1720))

    def test_token_cleanup_boundaries_latch_without_time_or_step_limit(self):
        for context_limit, processed_limit, totals, reason in [
            (1000, 0, [799, 800], 'max_context_tokens:800'),
            (0, 1250, [999, 1], 'max_processed_tokens:1000'),
        ]:
            with self.subTest(reason=reason), tempfile.TemporaryDirectory() as temp:
                root = Path(temp)
                events, state = root/'events', root/'state'
                event = lambda total: json.dumps({'type':'step_finish','part':{'tokens':{'total':total}}})+'\n'
                gate = proxy.CleanupGate(events,state,1000,900,60,context_limit,processed_limit)
                events.write_text(event(totals[0]))
                self.assertFalse(gate.cleanup(now=1000))
                events.write_text(event(totals[0])+event(totals[1]))
                self.assertTrue(gate.cleanup(now=1000))
                self.assertEqual(json.loads(state.read_text())['reason'],reason)
                events.write_text('')
                self.assertTrue(proxy.CleanupGate(events,state,1000,900,60,context_limit,processed_limit).cleanup(now=1000))

    def test_completed_token_usage_and_hard_limits_ignore_partial_events(self):
        with tempfile.TemporaryDirectory() as temp:
            events=Path(temp)/'events'
            events.write_text('\n'.join([
                json.dumps({'type':'step_finish','part':{'tokens':{'total':500,'cache':{'read':400}}}}),
                json.dumps({'type':'text','part':{'tokens':{'total':9999}}}),
                json.dumps({'type':'step_finish','part':{'tokens':{'total':700}}}),
                '{"type":"step_finish"',
            ]))
            usage=proxy.read_usage(events)
            self.assertEqual(usage,{'steps':2,'context_tokens':700,'processed_tokens':1200})
            self.assertEqual(proxy.token_limit_reason(usage,700,0),'max_context_tokens:700')
            self.assertEqual(proxy.token_limit_reason(usage,0,1200),'max_processed_tokens:1200')
            self.assertEqual(proxy.token_limit_reason(usage,701,1201),'')

    def test_refused_mutation_never_reaches_mcp_server(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            events, state = root/'events', root/'state'
            events.write_text('')
            server = root/'server.py'
            server.write_text('import sys,json\nfor line in sys.stdin:\n m=json.loads(line);print(json.dumps({"jsonrpc":"2.0","id":m["id"],"result":{"forwarded":m["params"]["name"]}}),flush=True)\n')
            request = lambda number, name: json.dumps({'jsonrpc':'2.0','id':number,'method':'tools/call','params':{'name':name}})+'\n'
            result = subprocess.run([sys.executable, str(ROOT/'scripts/native-execution-proxy.py'), '--events',str(events),'--state',str(state),'--started-at','0','--max-seconds','900','--max-steps','60','--',sys.executable,str(server)],
                                    input=request(1,'proofstorm_component_exec_live')+request(2,'proofstorm_lab_close'), text=True,capture_output=True,timeout=10,check=True)
            replies = {value['id']:value for value in map(json.loads,result.stdout.splitlines())}
            self.assertEqual(replies[1]['error']['data']['code'], 'cleanup_phase_only')
            self.assertEqual(replies[2]['result']['forwarded'], 'proofstorm_lab_close')
            self.assertTrue(state.exists())
            # A token boundary must enforce the same refusal before forwarding,
            # even when neither the wall clock nor step threshold has elapsed.
            import time
            state.unlink()
            events.write_text(json.dumps({'type':'step_finish','part':{'tokens':{'total':800}}})+'\n')
            result = subprocess.run([sys.executable,str(ROOT/'scripts/native-execution-proxy.py'),
                '--events',str(events),'--state',str(state),'--started-at',str(time.time()),
                '--max-seconds','900','--max-steps','60','--max-context-tokens','1000',
                '--',sys.executable,str(server)],
                input=request(1,'proofstorm_component_exec_live')+request(2,'proofstorm_lab_close'),
                text=True,capture_output=True,timeout=10,check=True)
            replies={value['id']:value for value in map(json.loads,result.stdout.splitlines())}
            self.assertEqual(replies[1]['error']['data']['code'],'cleanup_phase_only')
            self.assertEqual(replies[2]['result']['forwarded'],'proofstorm_lab_close')
            self.assertEqual(json.loads(state.read_text())['reason'],'max_context_tokens:800')

    def test_wait_crossing_boundary_announces_cleanup_without_mutation_probe(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            server = root/'server.py'
            server.write_text('''import sys,json,time
for line in sys.stdin:
 m=json.loads(line); time.sleep(2.1)
 doc={"terminal":False,"timed_out":True,"received":m["params"]["arguments"]}
 print(json.dumps({"jsonrpc":"2.0","id":m["id"],"result":{"content":[{"type":"text","text":json.dumps(doc)}],"structuredContent":doc}}),flush=True)
''')
            import time
            result = subprocess.run([sys.executable, str(ROOT/'scripts/native-execution-proxy.py'),
                '--events',str(root/'events'),'--state',str(root/'state'),
                '--started-at',str(time.time()),'--max-seconds','3','--max-steps','60',
                '--',sys.executable,str(server)], input=json.dumps({'jsonrpc':'2.0','id':1,
                'method':'tools/call','params':{'name':'proofstorm_operation_wait',
                'arguments':{'operation_id':'owned','timeout_seconds':45}}})+'\n',
                text=True,capture_output=True,timeout=10,check=True)
            reply = json.loads(result.stdout)['result']
            doc = json.loads(reply['content'][0]['text'])
            self.assertEqual(doc, reply['structuredContent'])
            self.assertEqual(doc['_benchmark_budget']['phase'], 'cleanup')
            self.assertLessEqual(doc['received']['timeout_seconds'], 2)
            self.assertFalse(doc['terminal'])
            self.assertTrue((root/'state').exists())

    def test_cleanup_wait_is_short_and_execution_deadline_is_unchanged(self):
        with tempfile.TemporaryDirectory() as temp:
            gate = proxy.CleanupGate(Path(temp)/'events',Path(temp)/'state',1000,600,50)
            def request(tool):
                return {'method':'tools/call','params':{'name':tool,'arguments':{'timeout_seconds':120}}}
            with patch.object(proxy.time, 'time', return_value=1491):
                wait = gate.bound_wait(request('proofstorm_operation_wait_many'))
                self.assertEqual(wait['params']['arguments']['timeout_seconds'],10)
                command = gate.bound_wait(request('proofstorm_component_exec_live'))
                self.assertEqual(command['params']['arguments']['timeout_seconds'],120)
            with patch.object(proxy.time, 'time', return_value=1598):
                wait = gate.bound_wait(request('proofstorm_lab_wait'))
                self.assertEqual(wait['params']['arguments']['timeout_seconds'],2)

    def test_early_step_cleanup_keeps_long_close_wait_without_extending_request(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            events = root/'events'
            events.write_text('\n'.join(json.dumps({'type':'step_finish'}) for _ in range(40)))
            gate = proxy.CleanupGate(events, root/'state', 1000, 600, 50)
            with patch.object(proxy.time, 'time', return_value=1250):
                for requested, expected in [(120,60),(60,60),(17,17),(1,1)]:
                    request = {'method':'tools/call','params':{'name':'proofstorm_lab_wait',
                        'arguments':{'instance_id':'owned','target_phase':'closed','timeout_seconds':requested}}}
                    bounded = gate.bound_wait(request)
                    self.assertEqual(bounded['params']['arguments']['timeout_seconds'],expected)
                    self.assertEqual(bounded['params']['arguments']['instance_id'],'owned')
                self.assertEqual(json.loads((root/'state').read_text())['reason'],'steps')
                self.assertFalse(gate.allows({'method':'tools/call','params':{'name':'proofstorm_component_exec_live'}}))

    def test_close_wait_preserves_report_margin_with_valid_minimum(self):
        with tempfile.TemporaryDirectory() as temp:
            gate = proxy.CleanupGate(Path(temp)/'events',Path(temp)/'state',1000,600,50)
            for now, expected in [(1550,20),(1550.2,19),(1569,1),(1570,1),(1598,1)]:
                with self.subTest(now=now), patch.object(proxy.time,'time',return_value=now):
                    request = {'method':'tools/call','params':{'name':'proofstorm_lab_wait',
                        'arguments':{'instance_id':'owned','target_phase':'closed','timeout_seconds':60}}}
                    bounded = gate.bound_wait(request)
                    self.assertEqual(bounded['params']['arguments']['timeout_seconds'],expected)

    def test_close_wait_exception_does_not_change_work_or_other_cleanup_waits(self):
        with tempfile.TemporaryDirectory() as temp:
            gate = proxy.CleanupGate(Path(temp)/'events',Path(temp)/'state',1000,600,50)
            request = {'method':'tools/call','params':{'name':'proofstorm_lab_wait',
                'arguments':{'instance_id':'owned','target_phase':'closed','timeout_seconds':120}}}
            with patch.object(proxy.time,'time',return_value=1477.4):
                self.assertEqual(gate.bound_wait(copy.deepcopy(request))['params']['arguments']['timeout_seconds'],3)
            with patch.object(proxy.time,'time',return_value=1490):
                for tool, arguments in [
                    ('proofstorm_lab_wait',{'instance_id':'owned','target_phase':'ready'}),
                    ('proofstorm_operation_wait',{'operation_id':'owned'}),
                    ('proofstorm_operation_wait_many',{'operation_ids':['owned']}),
                    ('proofstorm_candidate_wait',{'candidate_id':'owned'}),
                ]:
                    other = {'method':'tools/call','params':{'name':tool,
                        'arguments':{**arguments,'timeout_seconds':120}}}
                    self.assertEqual(gate.bound_wait(other)['params']['arguments']['timeout_seconds'],10)
                command = copy.deepcopy(request)
                command['params']['name'] = 'proofstorm_component_exec_live'
                self.assertEqual(gate.bound_wait(command)['params']['arguments']['timeout_seconds'],120)

    def test_invalid_wait_requests_are_not_repaired_or_crash_clamping(self):
        with tempfile.TemporaryDirectory() as temp:
            gate = proxy.CleanupGate(Path(temp)/'events',Path(temp)/'state',1000,600,50)
            base = {'method':'tools/call','params':{'name':'proofstorm_lab_wait',
                'arguments':{'instance_id':'owned','target_phase':'closed','timeout_seconds':60}}}
            malformed = [None, [], {}, {'method':'tools/call','params':None}]
            for arguments in [None, [], 'invalid', {}, {'timeout_seconds':None},
                              *({'timeout_seconds':v} for v in [True,False,0,-1,121,600,1.5,'60'])]:
                item = copy.deepcopy(base)
                item['params']['arguments'] = arguments
                malformed.append(item)
            with patch.object(proxy.time,'time',return_value=1490):
                for item in malformed:
                    with self.subTest(item=item):
                        self.assertEqual(gate.bound_wait(copy.deepcopy(item)),item)
                self.assertFalse(gate.allows({'method':'tools/call','params':None}))


if __name__ == '__main__':
    unittest.main()
