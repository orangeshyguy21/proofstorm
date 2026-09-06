"""Offline regressions for the failed pre-dispatch fixture contract."""
import importlib.util
from pathlib import Path
import unittest
import subprocess
import sys
import tempfile
import time

SPEC = importlib.util.spec_from_file_location('prefunder', Path(__file__).resolve().parents[1]
                                            / 'scripts/prepare-private-ecash-benchmark.py')
fixture = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(fixture)


class PrefunderTests(unittest.TestCase):
    def rpc_fixture(self, behavior, duration=1):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        client = fixture.Client.__new__(fixture.Client)
        client.output = Path(directory.name)
        client.identity = 0
        client.buffer = bytearray()
        client.deadline = time.monotonic() + duration
        client.process = subprocess.Popen([sys.executable, '-c',
            "import sys,json,time\nrequest=json.loads(sys.stdin.buffer.readline())\n" + behavior],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
        def close():
            if client.process.poll() is None:
                client.process.kill()
            client.process.wait(timeout=2)
            client.process.stdin.close()
            client.process.stdout.close()
        self.addCleanup(close)
        return client

    def test_notification_and_response_in_one_write(self):
        client = self.rpc_fixture("sys.stdout.write(json.dumps({'jsonrpc':'2.0','method':'notifications/progress','params':{}})+'\\n'+json.dumps({'jsonrpc':'2.0','id':request['id'],'result':{'ok':True}})+'\\n');sys.stdout.flush();time.sleep(.1)")
        self.assertEqual(client.rpc('tools/list', {}), {'ok': True})

    def test_delayed_notification_then_response(self):
        client = self.rpc_fixture("print(json.dumps({'jsonrpc':'2.0','method':'notifications/progress','params':{}}),flush=True);time.sleep(.04);print(json.dumps({'jsonrpc':'2.0','id':request['id'],'result':{}}),flush=True)")
        self.assertEqual(client.rpc('tools/list', {}), {})

    def test_eof_and_timeout_are_bounded(self):
        for behavior, duration, message in [('pass', 1, 'EOF'), ('time.sleep(2)', .08, 'deadline')]:
            client = self.rpc_fixture(behavior, duration)
            with self.assertRaisesRegex(RuntimeError, message):
                client.rpc('tools/list', {})

    def test_refusal_and_wrong_id_retained_without_request_body(self):
        for expression in ["{'jsonrpc':'2.0','id':request['id'],'error':{'code':-32602,'message':'tool not found'}}",
                           "{'jsonrpc':'2.0','id':999,'result':{}}"]:
            client = self.rpc_fixture('print(json.dumps(' + expression + '),flush=True)')
            with self.assertRaisesRegex(RuntimeError, 'refused'):
                client.rpc('tools/call', {'name': 'missing', 'arguments': {'private': 'canary'}})
            diagnostic = (client.output / 'setup-rpc-failure.json').read_text()
            self.assertNotIn('canary', diagnostic)

    def test_uses_advertised_batch_wait_and_validates_identity(self):
        client = fixture.Client.__new__(fixture.Client)
        calls = []
        def call(name, args):
            calls.append((name, args))
            return {'operations': [{'operation_id': 'setup-initialize', 'phase': 'succeeded'}]}
        client.call = call
        self.assertEqual(client.wait('setup-initialize')['phase'], 'succeeded')
        self.assertEqual(calls[0][0], 'proofstorm_operation_wait_many')
        with self.assertRaises(RuntimeError):
            client.wait('different-operation')

    def test_funding_matches_pinned_projection_without_coercing_unknowns(self):
        self.assertTrue(fixture.funding_settled({'status': 'SUCCEEDED', 'value_sat': '5000'}))
        for value in [5000, 5001, '5001', None, True]:
            self.assertFalse(fixture.funding_settled({'status': 'SUCCEEDED', 'value_sat': value}))
        self.assertFalse(fixture.funding_settled({'status': 'FAILED', 'value_sat': '5000'}))
