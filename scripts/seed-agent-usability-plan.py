#!/usr/bin/env python3
"""Create a fixture plan through MCP before a wallet-only benchmark session."""
import json
import os
from pathlib import Path
import select
import subprocess
import sys


def main():
    config_path, fixture_path, run_id = sys.argv[1:]
    config_path = Path(config_path)
    output = config_path.parent
    config = json.loads(config_path.read_text())['mcp']['proofstorm']
    request = json.loads(Path(fixture_path).read_text())
    request.update(plan_id=run_id + '-plan', idempotency_key=run_id + '-seed')
    (output / 'seed-plan.request.json').write_text(json.dumps(request, indent=2) + '\n')
    environment = {**os.environ, **config['environment']}
    environment['PROOFSTORM_CAPABILITIES'] = 'catalog.read,lab.create,lab.read'
    environment.pop('PROOFSTORM_CONTROL_NAMESPACE', None)
    with (output / 'seed-plan.stderr.log').open('w') as errors:
        process = subprocess.Popen(config['command'], env=environment,
                                   stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                   stderr=errors, text=True)
        def send(message):
            process.stdin.write(json.dumps(message) + '\n')
            process.stdin.flush()

        def rpc(identity, method, parameters):
            send({'jsonrpc': '2.0', 'id': identity, 'method': method, 'params': parameters})
            if not select.select([process.stdout], [], [], 60)[0]:
                raise TimeoutError('seed MCP response exceeded 60 seconds')
            response = json.loads(process.stdout.readline())
            if response.get('id') != identity or 'error' in response:
                raise RuntimeError(response)
            return response['result']

        try:
            rpc(1, 'initialize', {'protocolVersion': '2025-11-25', 'capabilities': {},
                                 'clientInfo': {'name': 'fixture-plan-seeder', 'version': '1'}})
            send({'jsonrpc': '2.0', 'method': 'notifications/initialized', 'params': {}})
            result = rpc(2, 'tools/call', {'name': 'proofstorm_lab_plan', 'arguments': request})
            if result.get('isError'):
                raise RuntimeError(result)
            receipt = result.get('structuredContent') or json.loads(result['content'][0]['text'])
            if not receipt['validation']['valid']:
                raise RuntimeError('seed plan did not validate')
            (output / 'seed-plan.json').write_text(json.dumps(receipt, indent=2) + '\n')
            print(json.dumps({'plan_id': receipt['plan_id'], 'plan_digest': receipt['plan_digest']}))
        finally:
            process.stdin.close()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()


if __name__ == '__main__':
    main()
