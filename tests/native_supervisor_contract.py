"""Run inside Linux with the static runner on PATH; never uses live funds."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time
import unittest

RUNNER = os.environ.get('PROOFSTORM_NATIVE_RUNNER', '/usr/local/bin/proofstorm-exec')


class SupervisorContract(unittest.TestCase):
    def setUp(self):
        self.root = Path(tempfile.mkdtemp(prefix='supervisor-test-'))
        self.handles = []

    def tearDown(self):
        for directory in self.handles:
            subprocess.run([RUNNER, 'cancel', str(directory)], capture_output=True, check=False)
            self.receipt(directory)
        shutil.rmtree(self.root)

    def start(self, argv=None, script='', timeout=2, output=None):
        directory = self.root / str(len(self.handles))
        directory.mkdir(mode=0o700)
        self.handles.append(directory)
        result = subprocess.run([RUNNER, 'start', str(directory)], input=json.dumps({
            'argv': argv or [], 'script': script, 'timeout_seconds':timeout,
            'output': output or {'mode':'private'}}), text=True,capture_output=True,check=True,timeout=5)
        self.assertEqual(json.loads(result.stdout), {'started':True})
        return directory

    def receipt(self, directory):
        deadline = time.monotonic()+10
        while time.monotonic() < deadline:
            data = json.loads(subprocess.check_output([RUNNER,'status',str(directory)]))
            if not data.get('running'):
                self.assertTrue(data.get('cleanup_verified'), data)
                return data
            time.sleep(0.03)
        self.fail('supervisor failed to produce bounded cleanup receipt')

    def test_private_capture_and_allowlisted_projection(self):
        canary = 'private-preimage-canary'
        code = 'import json;print(json.dumps(dict(status="SUCCEEDED",value_sat="700",payment_preimage="'+canary+'")))'
        private = self.start(['python3','-c',code])
        receipt = self.receipt(private)
        self.assertNotIn(canary, json.dumps(receipt))
        self.assertEqual(receipt['stdout'], '')
        self.assertIn(canary, (private/'stdout').read_text())
        self.assertEqual((private/'stdout').stat().st_mode & 0o777, 0o600)
        selected = self.receipt(self.start(['python3','-c',code],output={'mode':'json_fields','fields':['status','value_sat']}))
        self.assertEqual(selected['selected_output'], {'status':'SUCCEEDED','value_sat':'700'})
        self.assertNotIn(canary, json.dumps(selected))
        malformed = self.receipt(self.start(['printf','PREIMAGE '+canary],output={'mode':'json_fields','fields':['status']}))
        self.assertFalse(malformed['projection_succeeded'])
        self.assertNotIn(canary, json.dumps(malformed))

    def test_timeout_reaps_session_escaping_descendant(self):
        pidfile = self.root/'escaped-pid'
        grandchild = 'import os,time;open('+repr(str(pidfile))+',"w").write(str(os.getpid()));time.sleep(120)'
        parent = 'import subprocess,time;subprocess.Popen(["python3","-c",'+repr(grandchild)+'],start_new_session=True);time.sleep(120)'
        directory = self.start(['python3','-c',parent],timeout=1)
        receipt = self.receipt(directory)
        self.assertTrue(receipt['timed_out'])
        self.assertGreaterEqual(receipt['children_reaped'],2)
        with self.assertRaises(ProcessLookupError):
            os.kill(int(pidfile.read_text()),0)

    def test_cancel_is_scoped_and_reaps_owned_children(self):
        one = self.start(['sleep','120'],timeout=10)
        two = self.start(['sleep','120'],timeout=10)
        time.sleep(0.15)
        subprocess.check_call([RUNNER,'cancel',str(one)],stdout=subprocess.DEVNULL)
        self.assertTrue(self.receipt(one)['cancelled'])
        self.assertEqual(json.loads(subprocess.check_output([RUNNER,'status',str(two)])),{'running':True})
        subprocess.check_call([RUNNER,'cancel',str(two)],stdout=subprocess.DEVNULL)
        self.assertTrue(self.receipt(two)['cancelled'])

    def test_exit_scope_and_background_cleanup(self):
        direct = self.receipt(self.start(['sh','-c','exit 7']))
        self.assertEqual((direct['exit_code'],direct['exit_scope']),(7,'command'))
        shell = self.receipt(self.start(script='false | true'))
        self.assertEqual((shell['exit_code'],shell['exit_scope']),(0,'shell'))
        orphan = self.receipt(self.start(script='sleep 120 & exit 0'))
        self.assertEqual(orphan['exit_code'],0)
        self.assertGreaterEqual(orphan['children_reaped'],2)

    def test_large_streams_are_drained_but_retention_is_bounded(self):
        receipt = self.receipt(self.start(['python3','-c','import sys;sys.stdout.write("x"*100000);sys.stderr.write("y"*100000)']))
        self.assertTrue(receipt['output_truncated'])
        self.assertTrue(receipt['streams_complete'])
        for stream in ['stdout','stderr']:
            self.assertEqual(receipt['private_output'][stream]['bytes_observed'],100000)
            self.assertEqual(receipt['private_output'][stream]['retained_bytes'],16384)


if __name__ == '__main__':
    unittest.main()
