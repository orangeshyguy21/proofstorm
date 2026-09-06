import copy
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location('campaign', ROOT/'scripts/prepare-private-handoff-campaign.py')
campaign = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(campaign)


class OfflineHandoffTests(unittest.TestCase):
    def test_separate_identities_share_one_canonical_authority_with_bounded_grants(self):
        contract, configs = campaign.proposal(ROOT/'dev'/'..'/'dev'/'proposal', 'handoff-offline')
        source = configs['source']['mcp']['proofstorm']
        recipient = configs['recipient']['mcp']['proofstorm']
        a, b = source['environment'], recipient['environment']
        self.assertEqual(a['PROOFSTORM_DB'], b['PROOFSTORM_DB'])
        self.assertEqual(a['PROOFSTORM_DB'], str((ROOT/'dev/proposal/authority.sqlite3').resolve()))
        self.assertEqual(a['PROOFSTORM_WORKSPACE'], b['PROOFSTORM_WORKSPACE'])
        self.assertNotEqual(a['PROOFSTORM_PRINCIPAL'], b['PROOFSTORM_PRINCIPAL'])
        self.assertEqual(set(b['PROOFSTORM_CAPABILITIES'].split(',')), set(campaign.RECIPIENT_CAPABILITIES))
        self.assertNotIn('lease.acquire', b['PROOFSTORM_CAPABILITIES'])
        for config in configs.values():
            self.assertFalse(config['mcp']['proofstorm']['enabled'])
            self.assertTrue(all(v == 'deny' for v in config['permission'].values()))
        self.assertIsNone(contract['required_immutable_pins'])

    def test_stage_caps_fit_global_budget_without_borrowing_cleanup(self):
        contract, _ = campaign.proposal('/tmp/handoff-budget-test', 'handoff-offline')
        stages = contract['stage_limits']
        self.assertEqual(sum(s['seconds'] for s in stages), 600)
        self.assertEqual(sum(s['steps'] for s in stages), 50)
        self.assertEqual(sum(s['seconds'] for s in stages[:-1]), 450)
        self.assertEqual(sum(s['steps'] for s in stages[:-1]), 38)
        self.assertEqual(stages[-1]['role'], 'source')

    def test_packet_cannot_carry_payload_or_custom_command(self):
        packet = campaign.coordination_packet('handoff-offline', 'payload-'+'a'*64)
        self.assertEqual(packet['receive']['input'], {'kind': 'argv', 'index': 8})
        altered = copy.deepcopy(packet)
        altered['receive']['argv'][0] = 'changed'
        self.assertEqual(campaign.coordination_packet('handoff-offline', 'payload-'+'b'*64)['receive']['argv'][0], 'cdk-cli')
        for value in ['cashuAsecret', 'payload-'+'a'*63, {'token': 'secret'}, '../secret']:
            with self.assertRaises(ValueError):
                campaign.coordination_packet('handoff-offline', value)
        with self.assertRaises(ValueError):
            campaign.coordination_packet('../invalid', 'payload-'+'a'*64)

    def test_materializer_writes_only_disabled_proposals_and_refuses_overwrite(self):
        with tempfile.TemporaryDirectory() as temp:
            out = Path(temp)/'proposal'
            command = [sys.executable, str(ROOT/'scripts/prepare-private-handoff-campaign.py'),
                       '--run-id', 'handoff-offline', '--output', str(out)]
            result = subprocess.run(command, capture_output=True, text=True, check=True)
            self.assertEqual(json.loads(result.stdout)['mcp_started'], 0)
            before = {p.name: p.read_bytes() for p in out.iterdir()}
            self.assertEqual(len(before), 3)
            self.assertFalse((out/'authority.sqlite3').exists())
            self.assertNotEqual(subprocess.run(command, capture_output=True).returncode, 0)
            self.assertEqual(before, {p.name: p.read_bytes() for p in out.iterdir()})


if __name__ == '__main__':
    unittest.main()
