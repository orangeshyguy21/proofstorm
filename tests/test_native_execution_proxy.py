import importlib.util
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


if __name__ == '__main__':
    unittest.main()
