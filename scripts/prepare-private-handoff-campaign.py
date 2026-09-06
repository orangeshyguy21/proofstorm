#!/usr/bin/env python3
"""Materialize an OFFLINE proposal. Never starts MCP, models, or cluster work."""
import argparse
import copy
import json
from pathlib import Path
import re

ROOT = Path(__file__).resolve().parents[1]
MODEL = 'openrouter/moonshotai/kimi-k2.5'
RECIPIENT_CAPABILITIES = [
    'catalog.read', 'component.exec_live', 'wallet.control', 'experiment.read',
    'artifact.read', 'action.cancel',
]
RECEIVE = {
    'argv': ['cdk-cli', '--work-dir', '/wallet/cdk', '--unit', 'sat', '--non-interactive',
             'receive', '--allow-untrusted', '@proofstorm-private-input'],
    'timeout_seconds': 60, 'input': {'kind': 'argv', 'index': 8},
}
STAGES = [
    {'id': 'source-prepare', 'role': 'source', 'seconds': 150, 'steps': 12},
    {'id': 'recipient-receive', 'role': 'recipient', 'seconds': 180, 'steps': 16},
    {'id': 'source-revoke', 'role': 'source', 'seconds': 60, 'steps': 5},
    {'id': 'recipient-revoked', 'role': 'recipient', 'seconds': 60, 'steps': 5},
    {'id': 'source-finalize', 'role': 'source', 'seconds': 150, 'steps': 12},
]


def coordination_packet(run_id, reference):
    """Only fixed command metadata and a validated opaque reference cross roles.

    Live orchestration must obtain the reference from a verified durable capture
    receipt, not extract it from model prose. This function does not verify custody.
    """
    if not re.fullmatch(r'[a-zA-Z0-9][a-zA-Z0-9._-]{0,79}', run_id):
        raise ValueError('unsafe run ID')
    if not isinstance(reference, str) or not re.fullmatch(r'payload-[0-9a-f]{64}', reference):
        raise ValueError('invalid opaque reference')
    return {
        'instance_id': run_id + '-lab', 'experiment_id': run_id + '-experiment',
        'source_session_id': run_id + '-session', 'recipient_grant_id': run_id + '-recipient',
        'source_principal_id': 'benchmark-source', 'recipient_principal_id': 'benchmark-recipient',
        'source_wallet': 'wallet-a', 'component': 'wallet-b', 'mint': 'mint',
        'reference': reference, 'receive': copy.deepcopy(RECEIVE),
    }


def proposal(output, run_id):
    # The synthetic reference is validation-only and is never emitted as custody.
    coordination_packet(run_id, 'payload-' + '0' * 64)
    output = Path(output).resolve()
    base = json.loads((ROOT/'examples/opencode/proofstorm-only.json').read_text())
    configs = {}
    for role in ['source', 'recipient']:
        config = copy.deepcopy(base)
        config['model'] = MODEL
        server = config['mcp']['proofstorm']
        server.update(command=[str(ROOT/'target/release/proofstorm-mcp')], enabled=False)
        env = server['environment']
        env.update(PROOFSTORM_DB=str(output/'authority.sqlite3'),
                   PROOFSTORM_WORKSPACE='agent-usability-' + run_id,
                   PROOFSTORM_PRINCIPAL='benchmark-' + role,
                   PROOFSTORM_TOOLSET='experiment')
        if role == 'recipient':
            env['PROOFSTORM_CAPABILITIES'] = ','.join(RECIPIENT_CAPABILITIES)
        configs[role] = config
    contract = {
        'status': 'offline_proposal_not_dispatchable', 'run_id': run_id, 'model': MODEL,
        'required_gate_evidence': None, 'required_immutable_pins': None,
        'explicit_cluster_handoff_received': False,
        'authority_database': str(output/'authority.sqlite3'),
        'roles': {'source': 'benchmark-source', 'recipient': 'benchmark-recipient'},
        'stage_limits': STAGES, 'serial_only': True, 'model_contexts': 2,
        'limits': {'seconds': 600, 'steps': 50, 'context_tokens': 100000,
                   'processed_tokens': 3000000, 'equivalent_failures': 2},
        'cleanup': {'seconds': 480, 'steps': 40, 'context_tokens': 80000,
                    'processed_tokens': 2400000, 'owner': 'source',
                    'finalize_no_later_than_seconds': 450, 'finalize_no_later_than_steps': 38},
        'assistance': ['verified exact plan', 'protected setup', 'real 5000 sat Lightning prefunding',
                       'fixed approved native receive contract', 'metadata-only inter-role coordination'],
        'transfer': {'direction': 'cocod-to-cdk', 'amount_sat': 70, 'maximum_bytes': 65536,
                     'initial_balances': [5000, 0], 'final_balances': [4930, 70],
                     'receive': RECEIVE},
        'scope_limit': 'Source is trusted lab owner; no mutual wallet isolation claim.',
    }
    return contract, configs


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--run-id', required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    contract, configs = proposal(args.output, args.run_id)
    # Refuse overwriting an existing run or evidence directory.
    args.output.mkdir(parents=True, exist_ok=False)
    (args.output/'campaign-proposal.json').write_text(json.dumps(contract, indent=2)+'\n')
    for role, config in configs.items():
        (args.output/f'{role}.opencode.disabled.json').write_text(json.dumps(config, indent=2)+'\n')
    print(json.dumps({'status': contract['status'], 'output': str(args.output.resolve()),
                      'models_started': 0, 'mcp_started': 0, 'labs_created': 0}))


if __name__ == '__main__':
    main()
