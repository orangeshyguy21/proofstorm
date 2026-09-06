#!/usr/bin/env python3
"""Serial campaign engine; explicit one-campaign dispatch requires pinned gate evidence."""
import argparse
import copy
import hashlib
import json
import os
from pathlib import Path
import runpy
import selectors
import signal
import sqlite3
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]
PLAN = runpy.run_path(str(ROOT/'scripts/prepare-private-handoff-campaign.py'))
PROXY = runpy.run_path(str(ROOT/'scripts/native-execution-proxy.py'))
APPROVED_GATE = ROOT/'dev/wallet-integration-runs/private-handoff-02-20260906'
PINS = {
    'controller': 'sha256:3398ef33dddef3a3c5f9e1492fab6e4b667f003bf0f985258ea829cbf2f2aa38',
    'runner_sha256': '9368f0dd88aff029369f4af20fd6b67477b347ca89aa09c3d882886bea21006f',
    'mcp_release_sha256': '57bbad343d6d890a6fb23f5414bc55e888e4ba557d6a98af74bd423542c2e4b5',
}


class CampaignFailure(RuntimeError):
    pass


def save(path, value):
    temp = path.with_suffix(path.suffix + '.next')
    temp.write_text(json.dumps(value, indent=2)+'\n')
    temp.replace(path)


def native_ok(action):
    a = action['artifact']['content']
    expected = {'exit_code': 0, 'exit_signal': None, 'timed_out': False, 'cancelled': False,
                'cleanup_verified': True, 'streams_complete': True, 'output_truncated': False,
                'output_mode': 'private', 'stdout': '', 'stderr': '', 'private_files_retired': True,
                'runner_digest': 'sha256:'+PINS['runner_sha256']}
    if any(a.get(k) != v for k, v in expected.items()):
        raise CampaignFailure('native receipt incomplete or unsuccessful')


def approved_digest():
    r = PLAN['RECEIVE']
    # PrivateReceiveCommand's pinned serde field order, including default script.
    value = {'script': '', 'argv': r['argv'], 'timeout_seconds': r['timeout_seconds'], 'input': r['input']}
    return 'sha256:' + hashlib.sha256(json.dumps(value, separators=(',', ':')).encode()).hexdigest()


class EvidenceVerifier:
    """Read-only authority checks; no model prose becomes another role's instructions."""
    def __init__(self, run, run_id):
        self.run, self.run_id = Path(run), run_id
        self.workspace = 'agent-usability-' + run_id
        self.packet = None

    def rows(self, table):
        if table not in ('actions', 'experiment_leases'):
            raise CampaignFailure('unsupported authority table')
        with sqlite3.connect(f'file:{self.run}/authority.sqlite3?mode=ro', uri=True) as db:
            db.row_factory = sqlite3.Row
            return {r['id']: dict(r) for r in db.execute(
                f'SELECT * FROM {table} WHERE workspace_id=?', (self.workspace,))}

    def action(self, identity, principal):
        row = self.rows('actions').get(identity)
        if not row or row['principal_id'] != principal or json.loads(row['phase_json']) != 'succeeded':
            raise CampaignFailure('required terminal principal-owned action absent: ' + identity)
        row['artifact'] = json.loads(row['artifact_json'] or '{}')
        row['request'] = json.loads(row['request_json'])
        return row

    def refusal(self, events, identity, expected, operation=True):
        if operation and identity in self.rows('actions'):
            raise CampaignFailure('negative request created a journal action: ' + identity)
        calls = [e['part']['state'] for e in events if e.get('type') == 'tool_use'
                 and (e['part']['state'].get('input', {}).get('operation_id') == identity
                      or e['part']['state'].get('input', {}).get('idempotency_key') == identity)]
        if len(calls) != 1:
            raise CampaignFailure('negative request missing or repeated: ' + identity)
        state = calls[0]
        text = str(state.get('error', '')) + str(state.get('output', ''))
        if (not tool_failed(state) or expected not in text or 'lacks capability' in text
                or 'cleanup_phase_only' in text):
            raise CampaignFailure('expected authority refusal not established: ' + identity)

    def __call__(self, stage, events):
        source, recipient = 'benchmark-source', 'benchmark-recipient'
        leases = self.rows('experiment_leases')
        parent_id, child_id = self.run_id+'-lease', self.run_id+'-recipient'
        if stage == 'source-prepare':
            captured = self.action('source-capture', source)
            native_ok(captured)
            bound = self.action('source-handoff', source)['artifact']['content']['transfer']
            packet = PLAN['coordination_packet'](self.run_id, bound['id'])
            child = leases.get(child_id, {})
            scope = json.loads(child.get('delegation_json') or '{}')
            expected = {'parent_lease_id': parent_id, 'component': 'wallet-b', 'mint': 'mint',
                        'reference': bound['id'], 'receive_command_digest': approved_digest()}
            if (scope != expected or child.get('principal_id') != recipient
                    or json.loads(child.get('phase_json', 'null')) != 'active'
                    or child.get('expires_at', 0) <= time.time()
                    or bound.get('recipient', {}).get('principal') != recipient
                    or bound.get('recipient', {}).get('lease') != child_id
                    or bound.get('delivered') is not False
                    or bound.get('capture') != 'ready'
                    or captured['request'].get('private_payload', {}).get('reference') != bound['id']):
                raise CampaignFailure('delegated ready custody identity/command mismatch')
            self.packet = packet
            return packet
        if stage == 'recipient-receive':
            self.refusal(events, 'recipient-parent-release-denied', 'belongs to principal', operation=False)
            for identity in ['recipient-wallet-denied', 'recipient-command-denied']:
                self.refusal(events, identity, 'recipient lease does not authorize')
            if json.loads(leases[parent_id]['phase_json']) != 'active':
                raise CampaignFailure('forbidden parent release changed root authority')
            received = self.action('recipient-receive', recipient)
            native_ok(received)
            r = received['request']
            if (r.get('argv') != PLAN['RECEIVE']['argv'] or r.get('script', '') != ''
                    or r.get('timeout_seconds') != 60 or r.get('component') != 'wallet-b'
                    or r.get('lease_id') != child_id
                    or r.get('private_payload') != {'kind': 'consume', 'reference': self.packet['reference'],
                                                   'input': {'kind': 'argv', 'index': 8}}):
                raise CampaignFailure('actual receive differs from approved binding')
            balance = self.action('recipient-balance', recipient)
            a = balance['artifact']['content']
            if balance['accepted_at'] < received['completed_at'] or any(a.get(k) != v for k, v in {
                'balance_sat': 70, 'reserved_sat': 0, 'pending_sat': 0, 'pending_spent_sat': 0}.items()):
                raise CampaignFailure('post-receive destination balance unverified')
            return {**self.packet, 'observed_operation_ids': ['recipient-receive', 'recipient-balance']}
        if stage == 'source-revoke':
            if json.loads(leases[child_id]['phase_json']) != 'released':
                raise CampaignFailure('child revocation not recorded')
            return {**self.packet, 'child_released': True}
        if stage == 'recipient-revoked':
            self.refusal(events, 'recipient-revoked-balance', 'not active')
            return {**self.packet, 'revoked_request_refused': True}
        if stage == 'source-finalize':
            closed, exported = False, False
            for event in events:
                if event.get('type') != 'tool_use':
                    continue
                try:
                    value = json.loads(event['part']['state'].get('output', '{}'))
                except (ValueError, TypeError):
                    continue
                if isinstance(value, dict) and value.get('teardown_receipt', {}).get('verified_absent') is True:
                    closed = True
                if isinstance(value, dict) and value.get('journal_complete') is True:
                    exported = True
            if not closed or not any(e.get('type') == 'text' for e in events):
                raise CampaignFailure('source final report or observed close receipt missing')
            # Cleanup can succeed after an incomplete experiment. Keep its grade
            # separate rather than inventing final balance evidence.
            financial = False
            try:
                received = self.action('recipient-receive', recipient)
                for identity, expected in [
                    ('source-final-a', {'balance_sat': 4930, 'total_ready_sat': 4930,
                                        'reserved_sat': 0, 'inflight_sat': 0}),
                    ('source-final-b', {'balance_sat': 70, 'reserved_sat': 0,
                                        'pending_sat': 0, 'pending_spent_sat': 0}),
                ]:
                    observed = self.action(identity, source)
                    if observed['accepted_at'] < received['completed_at'] or any(
                            observed['artifact']['content'].get(k) != v for k, v in expected.items()):
                        raise CampaignFailure('fresh final balance mismatch')
                released = self.action('source-release', source)['artifact']['content']['transfer']
                if released['capture'] != 'released' or released['id'] != self.packet['reference']:
                    raise CampaignFailure('custody release missing')
                financial = all(json.loads(leases[i]['phase_json']) == 'released' for i in [parent_id, child_id])
            except (CampaignFailure, KeyError, TypeError):
                pass
            return {'agent_observed_closed': True, 'journal_exported': exported,
                    'final_balance_and_release_verified': financial}
        raise CampaignFailure('unknown stage')


INSTRUCTIONS = {
    'source-prepare': '''Use the assisted setup already completed under your source identity; never reinitialize, refund or acquire another root. Prepare one 65536-byte reference wallet-a→wallet-b (source-prepare). Native private capture source-capture: cocod send cashu 70 --mint-url http://mint:3338; timeout60, cashu_token format. Await terminal native receipt and ready capture. Delegate child <run>-recipient to benchmark-recipient for wallet-b/mint/exact reference, duration600,max_actions8. Approve the supplied receive contract exactly. Handoff under source-handoff to this child. Do not deliver or receive. Return the operation IDs; host validates durable metadata before switching roles.''',
    'recipient-receive': '''Read the supplied child lease; confirm its exact wallet/mint/reference and receive digest. Once each, test parent lease release (idempotency_key recipient-parent-release-denied), wrong wallet-a balance under child (recipient-wallet-denied), and private consume on wallet-b with only approved argv[0] changed to unapproved-receive (recipient-command-denied). Keep input index8 and timeout60 valid so this tests scope, not malformed arguments. If any is admitted, stop and return its handle without importing. After all three authority refusals, deliver once (recipient-deliver), then consume once (recipient-receive) with EXACT approved argv, private output and argv index8. Await terminal receipt including cleanup/retirement; then fresh passive wallet-b/mint balance (recipient-balance), require70 and zero reserved/pending/pending_spent. No source-wallet mutation, help, reconciliation, extra transfer or finalization. Report only your own observations and IDs.''',
    'source-revoke': '''Read recipient-receive and recipient-balance receipts yourself, verify terminal native completion and70sat destination. Read your parent lease to verify active after the forbidden release. Release the child only, idempotency_key source-revoke. Retain release receipt. No money mutation, second import or lab close yet.''',
    'recipient-revoked': '''The host has verified the child release. Once, call fresh wallet-b/mint balance under that revoked child with operation/idempotency ID recipient-revoked-balance. Require an authority refusal, no action. Report the actual private argv index8 receive binding, observed balance/receipt IDs and refusal outcomes. You have not observed source finalization. Do not claim exhaustive privacy or invent budget totals.''',
    'source-finalize': '''Cleanup has priority. If experimental work is incomplete, do not resume it. Observe/cancel owned accepted operations and retain terminal cleanup evidence. If still in work phase and recipient receive completed, request fresh balances source-final-a and source-final-b; expect4930/70 and zero supported unsettled categories. If budget forbids these, report missing observations. While root active, release any owned custody reference (source-release), await terminal receipt; release remaining child, release root, close experiment, export evidence, close lab, then lab_wait target closed timeout60. Observe verified_absent yourself and return a concise report of receipts/actual bindings/missing criteria. No guessed usage totals, no claim that revocation reversed money, no new receive/send/recovery.''',
}


def stage_prompt(stage, run_id, metadata, budget):
    return (INSTRUCTIONS[stage].replace('<run>', run_id) + '\nApproved receive metadata: '
            + json.dumps(PLAN['RECEIVE']) + '\nVerified coordination metadata: ' + json.dumps(metadata)
            + '\nShared absolute budget: ' + json.dumps(budget)
            + '\nUse only configured Proofstorm MCP. Token bytes/credentials remain private. Native exit, delivery and financial observations are separate evidence. '
              'This source is a trusted whole-lab owner. Final30seconds are reporting reserve. Respect _benchmark_budget on every reply. '
              'Two equivalent failures end experimental work; never replay an ambiguous accepted mutation. No guessed counters or broad secret-absence claims.')


def stop_process(process):
    if process.poll() is None:
        os.killpg(process.pid, signal.SIGTERM)
        try:
            process.wait(timeout=1)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait(timeout=1)


def tool_failed(state):
    if state.get('status') == 'error':
        return True
    try:
        value = json.loads(state.get('output', '{}'))
    except (TypeError, ValueError):
        return False
    return isinstance(value, dict) and (value.get('isError') is True or isinstance(value.get('error'), dict))
class SerialCampaign:
    """Engine is injectable for fake-process testing; no live CLI enables it yet."""
    def __init__(self, run, contract, command_factory, verifier, finalizer):
        self.run = Path(run)
        self.contract = contract
        self.command_factory, self.verify, self.finalizer = command_factory, verifier, finalizer
        self.events = self.run/'campaign.events.jsonl'
        self.latch = self.run/'cleanup-phase.json'
        self.sessions = {}
        self.attempted = set()
        self.results = []
        self.failures = []
        self.equivalent_errors = {}
        self.metadata = {'instance_id': contract['run_id']+'-lab', 'experiment_id': contract['run_id']+'-experiment',
                         'parent_lease_id': contract['run_id']+'-lease'}
        self.started = time.time()
        self.monotonic_started = time.monotonic()
        limits = contract['limits']
        self.stop_at = self.started + limits['seconds']
        self.monotonic_stop = self.monotonic_started + limits['seconds']
        self.gate = PROXY['CleanupGate'](self.events, self.latch, self.started, limits['seconds'],
            limits['steps'], limits['context_tokens'], limits['processed_tokens'])

    def cleanup(self, reason):
        if not self.latch.exists():
            save(self.latch, {'phase': 'cleanup', 'reason': reason, 'at_unix': time.time()})

    def usage(self):
        return PROXY['read_usage'](self.events)

    def hard_reason(self):
        usage = self.usage()
        limits = self.contract['limits']
        if time.monotonic() >= self.monotonic_stop or time.time() >= self.stop_at:
            return 'campaign_seconds'
        if usage['steps'] >= limits['steps']:
            return 'campaign_steps'
        return PROXY['token_limit_reason'](usage, limits['context_tokens'], limits['processed_tokens'])

    def run_stage(self, stage):
        role, name = stage['role'], stage['id']
        if role in self.attempted and role not in self.sessions:
            raise CampaignFailure('missing existing session ID; refusing a replacement context')
        start_steps = self.usage()['steps']
        end = min(self.stop_at, time.time()+stage['seconds'])
        # Work stages cannot consume source's reserved finalization window.
        if name != 'source-finalize':
            end = min(end, self.started+self.contract['cleanup']['finalize_no_later_than_seconds'])
        stage_budget = {'id': name, 'role': role, 'stop_at_unix': end,
                        'maximum_new_steps': stage['steps'], 'report_margin_seconds': 30,
                        'campaign_stop_at_unix': self.stop_at, 'cleanup_at_unix': self.started+480}
        save(self.run/'stage-budget.json', stage_budget)
        prompt = stage_prompt(name, self.contract['run_id'], self.metadata,
                              {**self.gate.budget(), 'stage': stage_budget})
        (self.run/f'{name}.prompt.txt').write_text(prompt)
        command, env = self.command_factory(stage, self.sessions.get(role), prompt, self.started)
        self.attempted.add(role)
        collected, reason, buffer = [], '', bytearray()
        monotonic_end = time.monotonic() + max(0, end-time.time())
        with (self.run/f'{name}.stderr.log').open('wb') as errors:
            process = subprocess.Popen(command, env={**os.environ, **env}, stdout=subprocess.PIPE,
                                       stderr=errors, start_new_session=True)
            selector = selectors.DefaultSelector()
            selector.register(process.stdout, selectors.EVENT_READ)
            try:
                with self.events.open('a') as aggregate, (self.run/f'{name}.events.jsonl').open('w') as local:
                    eof = False
                    while not eof:
                        reason = self.hard_reason()
                        usage = self.usage()
                        if not reason and (time.time() >= end or time.monotonic() >= monotonic_end):
                            reason = 'stage_seconds'
                        if not reason and usage['steps']-start_steps >= stage['steps']:
                            reason = 'stage_steps'
                        if name != 'source-finalize' and not reason:
                            if self.gate.cleanup() or usage['steps'] >= self.contract['cleanup']['finalize_no_later_than_steps']:
                                reason = 'source_cleanup_priority'
                        if reason:
                            break
                        for key, _ in selector.select(timeout=.05):
                            chunk = os.read(key.fd, 65536)
                            if not chunk:
                                eof = True
                                break
                            buffer.extend(chunk)
                            if len(buffer) > 4*1024*1024:
                                raise CampaignFailure('model event frame exceeded bound')
                            while b'\n' in buffer:
                                line, _, rest = buffer.partition(b'\n'); buffer = bytearray(rest)
                                event = json.loads(line)
                                identity = event.get('sessionID')
                                if identity:
                                    if (role in self.sessions and self.sessions[role] != identity) or any(
                                            other != role and value == identity for other, value in self.sessions.items()):
                                        raise CampaignFailure('model session crossed role boundary')
                                    self.sessions[role] = identity
                                event['_campaign_role'], event['_campaign_stage'] = role, name
                                encoded = json.dumps(event)+'\n'
                                aggregate.write(encoded); aggregate.flush(); local.write(encoded); local.flush()
                                collected.append(event)
                                if event.get('type') == 'step_finish':
                                    reason = self.hard_reason()
                                if event.get('type') == 'tool_use':
                                    state = event['part']['state']
                                    if tool_failed(state):
                                        # Fixed tool+argument hypothesis. IDs alone do not make a new attempt.
                                        args = {k: v for k, v in state.get('input', {}).items()
                                                if k not in ('operation_id', 'idempotency_key')}
                                        digest = hashlib.sha256(json.dumps([event['part']['tool'], args], sort_keys=True).encode()).hexdigest()
                                        self.equivalent_errors[digest] = self.equivalent_errors.get(digest, 0)+1
                                        if self.equivalent_errors[digest] >= 2:
                                            reason = 'equivalent_failure_limit'
                                if reason:
                                    break
                            if reason:
                                break
                        if reason:
                            break
                    if buffer and not reason:
                        reason = 'incomplete_event_frame'
                if reason:
                    stop_process(process)
                else:
                    try:
                        process.wait(timeout=max(.01, min(1, end-time.time())))
                    except subprocess.TimeoutExpired:
                        reason = 'process_did_not_exit'
                        stop_process(process)
                if not reason and process.returncode != 0:
                    reason = 'model_exit_nonzero'
                if not reason and role not in self.sessions:
                    reason = 'missing_session_identity'
            finally:
                selector.close()
                stop_process(process)
                process.stdout.close()
        if reason:
            raise CampaignFailure(reason)
        verified = self.verify(name, collected)
        self.metadata.update(verified)
        save(self.run/'coordination.json', self.metadata)
        return {'stage': name, 'role': role, 'status': 'verified', 'session_id': self.sessions[role]}

    def run_all(self):
        self.run.mkdir(parents=True, exist_ok=True)
        if self.events.exists():
            raise CampaignFailure('campaign already started; automatic replay forbidden')
        self.events.touch()
        save(self.run/'manifest.json', {'run_id': self.contract['run_id'],
             'workspace': 'agent-usability-'+self.contract['run_id'], 'started_at_unix': self.started,
             'limits': self.contract['limits'], 'model': self.contract['model']})
        try:
            for stage in self.contract['stage_limits']:
                if stage['id'] != 'source-finalize' and self.failures:
                    continue
                if self.hard_reason():
                    self.failures.append('hard_limit_before_'+stage['id'])
                    break
                try:
                    self.results.append(self.run_stage(stage))
                except Exception as error:
                    # Diagnostics are fixed exception category/reason, not raw private command output.
                    reason = str(error) if isinstance(error, CampaignFailure) else type(error).__name__
                    self.failures.append(stage['id']+':'+reason)
                    self.results.append({'stage': stage['id'], 'status': 'failed', 'reason': reason})
                    self.metadata.update(campaign_incomplete=True, failed_stage=stage['id'])
                    self.cleanup(reason)
        finally:
            # Always run the independent audit/finalizer, including failed source cleanup.
            campaign_elapsed = time.monotonic()-self.monotonic_started
            finalizer_started = time.monotonic()
            try:
                finalized = self.finalizer(self.run)
            except Exception as error:
                finalized = {'verified_idle': False, 'error_type': type(error).__name__}
            usage = self.usage()
            summary = {'stages': self.results, 'failures': self.failures, 'sessions': self.sessions,
                       'usage': usage, 'wall_seconds': time.time()-self.started,
                       'campaign_elapsed_seconds': campaign_elapsed,
                       'independent_finalizer_seconds': time.monotonic()-finalizer_started,
                       'finalizer': finalized, 'live_dispatch_enabled': False,
                       'target_property': 'held' if not self.failures and self.metadata.get('final_balance_and_release_verified')
                                          and self.metadata.get('journal_exported') and finalized.get('verified_idle') else 'incomplete',
                       'report_proficiency': 'needs_manual_review'}
            summary['execution_valid'] = not self.failures and finalized.get('verified_idle') is True and not finalized.get('operator_cleanup_required', False)
            summary['opencode_reported_cost'] = sum(json.loads(line).get('part', {}).get('cost', 0) or 0
                for line in self.events.read_text().splitlines() if json.loads(line).get('type') == 'step_finish')
            summary['billing_independently_verified'] = False
            save(self.run/'campaign-result.json', summary)
        return summary


def opencode_factory(run, configs):
    """Only called by a future authorized dispatch path; tests use fake factories."""
    run = Path(run)
    def build(stage, session, prompt, started):
        role = stage['role']; config = copy.deepcopy(configs[role])
        server = config['mcp']['proofstorm']; server['enabled'] = True
        server['command'] = [sys.executable, str(ROOT/'scripts/native-execution-proxy.py'),
            '--events', str(run/'campaign.events.jsonl'), '--state', str(run/'cleanup-phase.json'),
            '--started-at', str(started), '--max-seconds', '600', '--max-steps', '50',
            '--max-context-tokens', '100000', '--max-processed-tokens', '3000000',
            '--stage-budget', str(run/'stage-budget.json'), '--public-help-only', '--'] + server['command']
        path = run/f'{role}.opencode.json'; save(path, config)
        command = ['opencode', 'run', '--model', PLAN['MODEL'], '--format', 'json', '--print-logs']
        if session:
            command += ['--session', session]
        command += [prompt]
        return command, {'OPENCODE_CONFIG': str(path)}
    return build


def cluster_finalizer(run):
    """Audit always; retire only this workspace if agent finalization was incomplete."""
    audit = Path(run)/'operator-cluster-after.json'
    command = [sys.executable, str(ROOT/'scripts/agent-usability-cluster.py'), '--output', str(audit)]
    try:
        result = subprocess.run(command, capture_output=True, timeout=60)
        state = json.loads(audit.read_text()) if audit.exists() else {'verified_idle': False}
        idle = result.returncode == 0 and state.get('verified_idle')
    except (OSError, ValueError, subprocess.TimeoutExpired):
        state, idle = {'verified_idle': False}, False
    if not idle:
        try:
            subprocess.run(command + ['--cleanup-run', str(run), '--wait-seconds', '120'],
                           capture_output=True, timeout=180)
            state = json.loads(audit.read_text())
        except (OSError, ValueError, subprocess.TimeoutExpired) as error:
            state = {'verified_idle': False, 'error_type': type(error).__name__}
        state['operator_cleanup_required'] = True
    return state


def prerequisites(gate, binary, cluster):
    read = lambda name: json.loads((Path(gate)/(name+'.json')).read_text())
    pins, audit = read('build-pins'), read('receipt-audit')
    if any(pins.get(k) != v for k, v in PINS.items()):
        raise CampaignFailure('deterministic build pin mismatch')
    if hashlib.sha256(Path(binary).read_bytes()).hexdigest() != PINS['mcp_release_sha256']:
        raise CampaignFailure('release MCP pin mismatch')
    if (read('outcome').get('passed') is not True
            or read('closed').get('teardown_receipt', {}).get('verified_absent') is not True
            or read('cluster-after').get('remaining_labs_and_actions') != 0
            or read('cluster-after').get('instance_namespace_absent') is not True
            or audit.get('journal_complete') is not True
            or audit.get('all_native_cleanup_streams_and_runner_verified') is not True
            or audit.get('private_payload_streams_empty_and_files_retired') is not True):
        raise CampaignFailure('passing deterministic gate and teardown evidence required')
    expected = next((r for r in audit.get('recipient_command_bindings', [])
                     if r.get('operation_id') == 'handoff-out-receive'), {})
    if expected.get('command_digest') != approved_digest() or expected.get('input') != {'kind':'argv','index':8}:
        raise CampaignFailure('approved receive differs from deterministic binding')
    containers = [c for p in cluster.get('control_plane', []) for c in p.get('containers', [])
                  if c.get('name') == 'proofstormd']
    if (cluster.get('verified_idle') is not True or len(containers) != 1
            or containers[0].get('ready') is not True
            or not containers[0].get('image_id', '').endswith('@'+PINS['controller'])):
        raise CampaignFailure('idle cluster with exact ready controller required')
    return {'verified': True, 'pins': PINS, 'gate_path': str(gate),
            'gate_file_sha256': {f.name: hashlib.sha256(f.read_bytes()).hexdigest()
                                for f in Path(gate).glob('*.json') if f.name in
                                ['outcome.json','closed.json','cluster-after.json','receipt-audit.json','build-pins.json']}}


def dispatch(run_id):
    run = ROOT/'dev/agent-usability-runs'/run_id
    contract, configs = PLAN['proposal'](run, run_id)
    lock = Path(os.environ.get('TMPDIR', tempfile.gettempdir()))/'proofstorm-agent-usability-benchmark.lock'
    lock.mkdir()  # Same exclusive lock as the existing single-session runner.
    cleanup_needed, finalized = False, False
    def finalize(path):
        nonlocal finalized
        finalized = True
        return cluster_finalizer(path)
    try:
        run.mkdir(parents=True, exist_ok=False)
        save(run/'manifest.json', {'run_id':run_id, 'workspace':'agent-usability-'+run_id, 'setup_only':True})
        audit_path = run/'cluster-before.json'
        subprocess.run([sys.executable, str(ROOT/'scripts/agent-usability-cluster.py'),
                        '--output', str(audit_path)], check=True, capture_output=True, timeout=60)
        checked = prerequisites(APPROVED_GATE, ROOT/'target/release/proofstorm-mcp', json.loads(audit_path.read_text()))
        save(run/'dispatch-prerequisites.json', checked)
        # Explicit coordinator handback authorizes ONE campaign. No retry flag.
        authorization = ROOT/'dev/agent-usability-runs/private-handoff-authorization-20260906.json'
        with authorization.open('x') as stream:
            json.dump({'run_id':run_id, 'gate':str(APPROVED_GATE), 'pins':PINS,
                       'authorized_campaigns':1, 'consumed_at_unix':time.time()}, stream)
        save(run/'campaign-proposal.json', contract)
        paths = {}
        for role, config in configs.items():
            paths[role] = run/f'{role}.bootstrap.json'
            save(paths[role], config)
        owned = [ROOT/'scripts/run-private-handoff-campaign.py', ROOT/'scripts/prepare-private-handoff-campaign.py',
                 ROOT/'scripts/prepare-private-ecash-benchmark.py', ROOT/'scripts/seed-agent-usability-plan.py',
                 ROOT/'scripts/native-execution-proxy.py', ROOT/'scripts/agent-usability-cluster.py',
                 ROOT/'scripts/fixtures/private-ecash-verified-plan.json']
        (run/'harness').mkdir()
        for path in owned:
            (run/'harness'/path.name).write_bytes(path.read_bytes())
        save(run/'harness-digests.json', {str(p.relative_to(ROOT)):hashlib.sha256(p.read_bytes()).hexdigest() for p in owned})
        with (run/'source-diff.patch').open('wb') as stream:
            subprocess.run(['git', 'diff', '--binary', 'HEAD'], cwd=ROOT, stdout=stream, check=True)
        try:
            cleanup_needed = True
            with (run/'setup-driver.log').open('wb') as log:
                subprocess.run([sys.executable, str(ROOT/'scripts/seed-agent-usability-plan.py'),
                    str(paths['source']), str(ROOT/'scripts/fixtures/private-ecash-verified-plan.json'), run_id],
                    stdout=log, stderr=subprocess.STDOUT, check=True, timeout=120)
                # Provision recipient grants without a model. No lab mutation.
                provision = run/'recipient-provision'; provision.mkdir()
                Client = runpy.run_path(str(ROOT/'scripts/prepare-private-ecash-benchmark.py'))['Client']
                client = Client(configs['recipient']['mcp']['proofstorm'], provision)
                try:
                    tools = client.rpc('tools/list', {})
                    names = {t['name'] for t in tools['tools']}
                    if not {'proofstorm_wallet_balance','proofstorm_component_exec_live','proofstorm_private_transfer'}.issubset(names):
                        raise CampaignFailure('recipient tool profile incomplete')
                finally:
                    client.close()
                subprocess.run([sys.executable, str(ROOT/'scripts/prepare-private-ecash-benchmark.py'),
                    str(paths['source']), run_id], stdout=log, stderr=subprocess.STDOUT, check=True, timeout=660)
        except BaseException:
            save(run/'setup-dispatch-failure.json', {'model_dispatched':False, 'finalizer':finalize(run)})
            raise
        verifier = EvidenceVerifier(run, run_id)
        for receipt in (run/'setup').glob('*.json'):
            value = json.loads(receipt.read_text()).get('artifact', {}).get('content', {})
            if value.get('supervisor_version') and value.get('runner_digest') != 'sha256:'+PINS['runner_sha256']:
                save(run/'setup-dispatch-failure.json', {'model_dispatched':False, 'reason':'setup_runner_pin_mismatch',
                                                       'finalizer':finalize(run)})
                raise CampaignFailure('setup runner pin mismatch')
        engine = SerialCampaign(run, contract, opencode_factory(run, configs), verifier, finalize)
        engine.metadata.update(assisted_initial_balances=[5000,0],
                               setup_operation_ids=['setup-funding','setup-issuance','setup-balance-a','setup-balance-b'])
        result = engine.run_all()
        result['live_dispatch_enabled'] = True
        save(run/'campaign-result.json', result)
        # Session exports happen after the shared model clock; no model resumes.
        for role, identity in engine.sessions.items():
            with (run/f'{role}.session.json').open('wb') as out, (run/f'{role}.export.log').open('wb') as error:
                subprocess.run(['opencode','export',identity], stdout=out, stderr=error, timeout=60)
        return result
    finally:
        try:
            if cleanup_needed and not finalized:
                save(run/'dispatch-finalizer.json', finalize(run))
        finally:
            lock.rmdir()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--dispatch-approved-campaign', action='store_true')
    parser.add_argument('--run-id')
    args = parser.parse_args()
    if not args.dispatch_approved_campaign:
        print(json.dumps({'live_dispatch_enabled':False, 'reason':'Explicit approved-campaign dispatch required; default does not launch.'}))
        return 2
    if not args.run_id:
        parser.error('--run-id is required')
    result = dispatch(args.run_id)
    print(json.dumps(result))
    return 0 if result['execution_valid'] and result['target_property']=='held' else 1


if __name__ == '__main__':
    raise SystemExit(main())
