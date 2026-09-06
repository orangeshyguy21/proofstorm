#!/usr/bin/env python3
"""One assisted prefunding setup; no wallet mutation is retried on failure."""
import json
import os
from pathlib import Path
import select
import subprocess
import sys
import time

API = """import json,urllib.request,secrets,time
from pathlib import Path
root=Path('/wallet/.cocod')
def api(path,body=None):
 key=(root/'credentials/current/client').read_text().strip()
 req=urllib.request.Request('http://127.0.0.1:62626'+path,data=None if body is None else json.dumps(body).encode(),headers={'Authorization':'Bearer '+key,'Content-Type':'application/json'})
 with urllib.request.urlopen(req,timeout=20) as response: return json.load(response)
"""


def funding_settled(selected):
    # The pinned LND JSON projection deliberately retains its numeric string.
    return selected == {'status': 'SUCCEEDED', 'value_sat': '5000'}


class Client:
    def __init__(self, config, output):
        self.errors = (output / 'setup-mcp.stderr.log').open('w')
        self.process = subprocess.Popen(config['command'], env={**os.environ, **config['environment']},
                                        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=self.errors)
        self.identity = 0
        self.output = output
        self.buffer = bytearray()
        self.deadline = time.monotonic() + 600
        self.rpc('initialize', {'protocolVersion': '2025-11-25', 'capabilities': {},
                               'clientInfo': {'name': 'private-ecash-prefunder', 'version': '1'}})
        self.send({'jsonrpc': '2.0', 'method': 'notifications/initialized', 'params': {}})

    def send(self, message):
        self.process.stdin.write((json.dumps(message) + '\n').encode())
        self.process.stdin.flush()

    def rpc(self, method, params):
        self.identity += 1
        self.send({'jsonrpc': '2.0', 'id': self.identity, 'method': method, 'params': params})
        deadline = min(time.monotonic() + 90, self.deadline)
        while True:
            # Drain complete buffered frames before polling the descriptor. A
            # notification and its response may arrive in the same OS read.
            if time.monotonic() >= deadline:
                raise RuntimeError('setup RPC deadline; original mutations must not be retried')
            if b'\n' not in self.buffer:
                if not select.select([self.process.stdout], [], [], deadline - time.monotonic())[0]:
                    raise RuntimeError('setup RPC deadline; original mutations must not be retried')
                chunk = os.read(self.process.stdout.fileno(), 65536)
                if not chunk:
                    raise RuntimeError('setup MCP EOF; original mutations must not be retried')
                self.buffer.extend(chunk)
                if len(self.buffer) > 4 * 1024 * 1024:
                    raise RuntimeError('setup MCP frame exceeded bounded capture')
                continue
            line, _, remainder = self.buffer.partition(b'\n')
            self.buffer = bytearray(remainder)
            response = json.loads(line)
            if 'id' not in response and 'method' in response:
                continue
            break
        if response.get('id') != self.identity or 'error' in response:
            (self.output / 'setup-rpc-failure.json').write_text(json.dumps({
                'method': method, 'tool': params.get('name'),
                'expected_id': self.identity, 'observed_id': response.get('id'),
                'error_code': response.get('error', {}).get('code')}, indent=2) + '\n')
            raise RuntimeError('setup RPC refused; inspect retained MCP diagnostics')
        return response['result']

    def call(self, name, arguments):
        result = self.rpc('tools/call', {'name': name, 'arguments': arguments})
        if result.get('isError'):
            raise RuntimeError('setup tool refused: ' + name)
        return result.get('structuredContent') or json.loads(result['content'][0]['text'])

    def wait(self, identity):
        result = self.call('proofstorm_operation_wait_many',
                           {'operation_ids': [identity], 'timeout_seconds': 60})
        operations = result.get('operations', [])
        if len(operations) != 1 or operations[0].get('operation_id') != identity:
            raise RuntimeError('setup batch wait identity mismatch')
        operation = operations[0]
        if result.get('artifact_bodies_omitted'):
            return self.call('proofstorm_operation_status', {'operation_id': identity})
        return operation

    def close(self):
        self.process.stdin.close()
        try:
            self.process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait()
        self.errors.close()


def main():
    config_path, run_id = sys.argv[1:]
    path = Path(config_path)
    output = path.parent
    setup = output / 'setup'
    setup.mkdir()
    config = json.loads(path.read_text())['mcp']['proofstorm']
    workspace = config['environment']['PROOFSTORM_WORKSPACE']
    # Enables existing owned-workspace finalizer if preparation fails before the model manifest.
    (output / 'manifest.json').write_text(json.dumps({'workspace': workspace, 'run_id': run_id,
                                                    'setup_only': True}))
    scope = {'instance_id': run_id + '-lab', 'experiment_id': run_id + '-experiment',
             'lease_id': run_id + '-lease'}
    ids = []
    client = Client(config, output)

    def save(name, value):
        (setup / (name + '.json')).write_text(json.dumps(value, indent=2) + '\n')

    def call(name, args, label):
        result = client.call(name, args)
        save(label, result)
        return result

    def operation(tool, label, args):
        identity = 'setup-' + label
        ids.append(identity)
        result = call(tool, {**scope, 'operation_id': identity, 'idempotency_key': identity, **args},
                      label + '-submitted')
        while result.get('phase') in ('pending', 'running'):
            result = client.wait(identity)
            save(label, result)
        if result.get('phase') != 'succeeded':
            raise RuntimeError('setup operation failed: ' + identity)
        return result.get('artifact', {}).get('content', {})

    def native(label, component, argv, mode=None):
        receipt = operation('proofstorm_component_exec_live', label, {
            'component': component, 'argv': argv, 'timeout_seconds': 60,
            'output': mode or {'mode': 'private'}})
        expected = {'exit_code': 0, 'timed_out': False, 'cancelled': False,
                    'cleanup_verified': True, 'streams_complete': True, 'output_truncated': False,
                    'stdout': '', 'stderr': ''}
        if any(receipt.get(k) != v for k, v in expected.items()):
            raise RuntimeError('setup native receipt failed: ' + label)
        return receipt

    try:
        advertised = {tool['name'] for tool in client.rpc('tools/list', {})['tools']}
        required = {'proofstorm_lab_apply', 'proofstorm_lab_wait', 'proofstorm_experiment_create',
                    'proofstorm_lease_acquire', 'proofstorm_component_exec_live',
                    'proofstorm_operation_wait_many', 'proofstorm_operation_status',
                    'proofstorm_component_restart', 'proofstorm_liquidity_bootstrap', 'proofstorm_wallet_balance'}
        if required - advertised:
            raise RuntimeError('setup tool profile missing: ' + ','.join(sorted(required - advertised)))
        seed = json.loads((output / 'seed-plan.json').read_text())
        call('proofstorm_lab_apply', {'plan_id': seed['plan_id'], 'expected_plan_digest': seed['plan_digest'],
                                    'instance_id': scope['instance_id'], 'idempotency_key': 'setup-apply'}, 'applied')
        ready = call('proofstorm_lab_wait', {'instance_id': scope['instance_id'], 'target_phase': 'ready',
                                           'timeout_seconds': 60}, 'ready')
        if ready.get('phase') != 'ready':
            raise RuntimeError('prefunding lab not ready')
        call('proofstorm_experiment_create', {k: v for k, v in {**scope, 'idempotency_key': 'setup-experiment'}.items()
                                             if k != 'lease_id'}, 'experiment')
        lease = call('proofstorm_lease_acquire', {'experiment_id': scope['experiment_id'], 'lease_id': scope['lease_id'],
                                                'duration_seconds': 1800, 'max_actions': 100,
                                                'idempotency_key': 'setup-lease'}, 'lease')
        native('initialize', 'wallet-a', ['python3', '-c', API + "p=Path('/wallet/session.passphrase'); p.write_text(secrets.token_urlsafe(32)); p.chmod(0o600)\nr=api('/v1/admin/wallet/initialize',{'passphrase':p.read_text()}); assert r['generatedMnemonic']\nconfig=root/'config.json'; settings=json.loads(config.read_text()); settings['mintUrl']='http://mint:3338'; config.write_text(json.dumps(settings)); config.chmod(0o600)"])
        operation('proofstorm_component_restart', 'restart', {'component': 'wallet-a'})
        native('unlock', 'wallet-a', ['python3', '-c', API + "api('/v1/admin/session/start',{'passphrase':Path('/wallet/session.passphrase').read_text()})\ndeadline=time.monotonic()+40\nwhile time.monotonic()<deadline:\n if api('/v1/status')['cocoSession']['state']=='running': break\n time.sleep(.25)\nelse: raise RuntimeError('session_not_running')"])
        operation('proofstorm_liquidity_bootstrap', 'liquidity', {'chain': 'chain', 'mint_lightning': 'mint-lnd',
                  'payer_lightning': 'payer-lnd', 'funding_sat': 50000000, 'channel_sat': 10000000, 'push_sat': 5000000})
        invoice = native('invoice', 'wallet-a', ['cocod', 'receive', 'bolt11', '5000', '--mint-url', 'http://mint:3338'], {'mode': 'bolt11'})
        selected = invoice.get('selected_output', {})
        if (invoice.get('projection_succeeded') is not True or selected.get('amount_msat') != 5000000
                or selected.get('currency') != 'bcrt' or selected.get('expires_at_unix', 0) <= time.time()):
            raise RuntimeError('setup structured invoice invalid')
        payment = native('funding', 'payer-lnd', ['lncli', '--lnddir=/home/lnd/.lnd', '--network=regtest',
                         '--rpcserver=127.0.0.1:10009', 'payinvoice', '--force', '--json', selected['payment_request']],
                         {'mode': 'json_fields', 'fields': ['status', 'value_sat']})
        if not funding_settled(payment.get('selected_output')):
            raise RuntimeError('setup funding settlement unverified')
        native('issuance', 'wallet-a', ['python3', '-c', API + "deadline=time.monotonic()+40\nwhile time.monotonic()<deadline:\n if api('/balance')['output'].get('http://mint:3338',{}).get('sats')==5000: break\n time.sleep(.5)\nelse: raise RuntimeError('issuance_not_observed')"])
        # CDK requires a native initialization before its fail-closed passive adapter can open state.
        native('cdk-initialize', 'wallet-b', ['cdk-cli', '--work-dir', '/wallet/cdk', '--unit', 'sat',
                                           '--non-interactive', 'balance'])
        a = operation('proofstorm_wallet_balance', 'balance-a', {'wallet': 'wallet-a', 'mint': 'mint'})
        b = operation('proofstorm_wallet_balance', 'balance-b', {'wallet': 'wallet-b', 'mint': 'mint'})
        if any(a.get(k) != v for k, v in {'balance_sat': 5000, 'total_ready_sat': 5000, 'reserved_sat': 0, 'inflight_sat': 0}.items()):
            raise RuntimeError('setup cocod balance not verified')
        if any(b.get(k) != 0 for k in ['balance_sat', 'reserved_sat', 'pending_sat', 'pending_spent_sat']):
            raise RuntimeError('setup CDK zero balance not verified')
        handoff = {**scope, 'principal': config['environment']['PROOFSTORM_PRINCIPAL'], 'lease_expires_at_unix': lease['expires_at_unix'],
                   'assisted_planning_and_prefunding': True, 'setup_operation_ids': ids,
                   'initial_balances': {'wallet-a': a, 'wallet-b': b},
                   'mint_url': 'http://mint:3338', 'setup_receipts': 'setup/*.json',
                   'instructions': 'Continue this exact experiment and lease; prefix model action IDs with agent-. Do not initialize or fund again.'}
        (output / 'setup-handoff.json').write_text(json.dumps(handoff, indent=2) + '\n')
        print(json.dumps({'setup_verified': True, 'operations': len(ids), **scope}), flush=True)
    except Exception as error:
        save('failure', {'setup_verified': False, 'error': str(error), 'operation_ids': ids,
                         'model_dispatched': False})
        client.close()
        client = None
        subprocess.run([sys.executable, str(Path(__file__).with_name('agent-usability-cluster.py')),
                        '--cleanup-run', str(output), '--wait-seconds', '120',
                        '--output', str(output / 'setup-failure-cluster-audit.json')], check=False)
        raise SystemExit('Prefunding failed; no model dispatched. Inspect setup/failure.json.') from None
    finally:
        if client:
            client.close()


if __name__ == '__main__':
    main()
