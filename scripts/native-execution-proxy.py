#!/usr/bin/env python3
"""Benchmark-only MCP admission gate. Cleanup is latched and survives reconnects."""
import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import signal
import subprocess
import sys
import threading
import time

CLEANUP_TOOLS = frozenset('''proofstorm_workspace_read
proofstorm_lab_status proofstorm_lab_wait proofstorm_lab_close proofstorm_lab_close_wait
proofstorm_lab_component_status_list proofstorm_lab_component_status_read
proofstorm_lab_inventory_list proofstorm_operation_status proofstorm_operation_wait
proofstorm_operation_wait_many proofstorm_action_list proofstorm_action_cancel
proofstorm_artifact_read proofstorm_artifact_export proofstorm_artifact_list
proofstorm_private_access_revoke proofstorm_private_access_read proofstorm_session_list proofstorm_session_finish proofstorm_experiment_close proofstorm_experiment_read
proofstorm_candidate_cancel proofstorm_candidate_read proofstorm_candidate_list
proofstorm_candidate_wait'''.split())
WAIT_TOOLS = frozenset('''proofstorm_lab_wait
proofstorm_operation_wait proofstorm_operation_wait_many proofstorm_candidate_wait'''.split())

CDK_PREFIX = ('cdk-cli', '--work-dir', '/wallet/cdk', '--unit', 'sat', '--non-interactive')
SAFE_PUBLIC_ARGV = frozenset([
    ('cocod', '--help'), ('cocod', '--version'), ('cocod', 'help'),
    ('cocod', 'send', '--help'), ('cocod', 'send', 'cashu', '--help'),
    ('cocod', 'receive', 'cashu', '--help'),
    ('cdk-cli', '--help'), ('cdk-cli', '--version'), ('cdk-cli', 'help'),
    CDK_PREFIX + ('--help',), CDK_PREFIX + ('--version',),
    CDK_PREFIX + ('send', '--help'), CDK_PREFIX + ('receive', '--help'),
    CDK_PREFIX + ('check-pending', '--help'),
])


def public_output_allowed(message):
    params = message.get('params', {})
    if message.get('method') != 'tools/call' or not isinstance(params, dict) or params.get('name') != 'proofstorm_component_exec_live':
        return True
    args = params.get('arguments', {})
    if not isinstance(args, dict) or not isinstance(args.get('output'), dict) or args['output'].get('mode') != 'public':
        return True  # The MCP validates malformed requests; private native execution stays generic.
    argv = args.get('argv')
    return (not args.get('script') and isinstance(argv, list)
            and all(isinstance(x, str) for x in argv) and tuple(argv) in SAFE_PUBLIC_ARGV)


def argument_snapshot(message, boundary):
    """Fixed custody metadata only; no native commands or arbitrary string values."""
    params = message.get('params', {})
    if message.get('method') != 'tools/call' or not isinstance(params, dict) or params.get('name') != 'proofstorm_private_transfer':
        return None
    args = params.get('arguments', {})
    if not isinstance(args, dict):
        return None
    transfer = args.get('transfer', {})
    transfer = transfer if isinstance(transfer, dict) else {}
    fields = {}
    for key in ['transferMethod', 'component', 'destinationComponent', 'maximumBytes', 'reference']:
        value = transfer.get(key)
        allowed = (value in ['prepare', 'status', 'deliver', 'release'] if key == 'transferMethod'
                   else value in ['wallet-a', 'wallet-b'] if key in ['component', 'destinationComponent']
                   else type(value) is int and 0 <= value <= 1048576 if key == 'maximumBytes'
                   else isinstance(value, str) and value.startswith('payload-') and len(value) == 72
                   and all(c in '0123456789abcdef' for c in value[8:]))
        fields[key] = ({'state': 'missing'} if key not in transfer else {'state': 'null'} if value is None
                       else {'state': 'allowed', 'value': value} if allowed else {'state': 'withheld'})
    identity = args.get('operation_id')
    return {'boundary': boundary, 'at_unix': time.time(),
            'operation_id_sha256': hashlib.sha256(identity.encode()).hexdigest() if isinstance(identity, str) else None,
            'fields': fields}


def read_usage(events):
    usage = {'steps': 0, 'context_tokens': 0, 'processed_tokens': 0}
    try:
        with Path(events).open() as stream:
            for line in stream:
                try:
                    event = json.loads(line)
                except ValueError:
                    continue  # A concurrently written event may be incomplete.
                if event.get('type') != 'step_finish':
                    continue
                usage['steps'] += 1
                total = event.get('part', {}).get('tokens', {}).get('total', 0)
                if type(total) is int and total >= 0:
                    usage['context_tokens'] = max(usage['context_tokens'], total)
                    usage['processed_tokens'] += total
    except FileNotFoundError:
        pass
    return usage


def token_limit_reason(usage, max_context_tokens=0, max_processed_tokens=0):
    for name, limit in [('context_tokens', max_context_tokens), ('processed_tokens', max_processed_tokens)]:
        if limit and usage[name] >= limit:
            return f'max_{name}:{limit}'
    return ''


class CleanupGate:
    def __init__(self, events, state, started_at, max_seconds, max_steps,
                 max_context_tokens=0, max_processed_tokens=0, stage_budget=None):
        self.events = Path(events)
        self.state = Path(state)
        self.started_at = started_at
        self.seconds = max(1, max_seconds * 80 // 100)
        self.steps = max(1, max_steps * 80 // 100)
        self.max_seconds = max_seconds
        self.max_context_tokens = max_context_tokens
        self.max_processed_tokens = max_processed_tokens
        self.stage_budget = Path(stage_budget) if stage_budget else None

    def cleanup(self, now=None):
        if self.state.exists():
            return True
        elapsed = (time.time() if now is None else now) - self.started_at
        usage = read_usage(self.events)
        token_reason = token_limit_reason(
            usage, max(1, self.max_context_tokens * 80 // 100) if self.max_context_tokens else 0,
            max(1, self.max_processed_tokens * 80 // 100) if self.max_processed_tokens else 0)
        if elapsed < self.seconds and usage['steps'] < self.steps and not token_reason:
            return False
        receipt = {'phase': 'cleanup', 'reason': 'time' if elapsed >= self.seconds else
                   'steps' if usage['steps'] >= self.steps else token_reason,
                   'observed_steps': usage['steps'], 'elapsed_seconds': elapsed,
                   'cleanup_after_steps': self.steps, 'cleanup_after_seconds': self.seconds,
                   'observed_token_usage': usage}
        try:
            fd = os.open(self.state, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
            with os.fdopen(fd, 'w') as out:
                json.dump(receipt, out)
        except FileExistsError:
            pass
        return True

    def allows(self, message):
        if message.get('method') != 'tools/call':
            return True
        if not self.cleanup():
            return True
        params = message.get('params')
        if not isinstance(params, dict):
            return False
        if params.get('name') in CLEANUP_TOOLS:
            return True
        # Custody retirement is cleanup. Keep authorization in MCP and admit
        # only these methods; delivery, handoff and reservation remain work.
        if params.get('name') == 'proofstorm_private_transfer':
            args = params.get('arguments')
            transfer = args.get('transfer') if isinstance(args, dict) else None
            return isinstance(transfer, dict) and transfer.get('transferMethod') in ('status', 'release')
        return False

    def budget(self, now=None):
        now = time.time() if now is None else now
        cleanup = self.cleanup(now)
        result = {'phase': 'cleanup' if cleanup else 'work',
                'observed_usage': read_usage(self.events),
                'max_context_tokens': self.max_context_tokens or None,
                'max_processed_tokens': self.max_processed_tokens or None,
                'now_unix': round(now, 3),
                'cleanup_at_unix': self.started_at + self.seconds,
                'hard_stop_at_unix': self.started_at + self.max_seconds,
                'seconds_to_cleanup': max(0, round(self.started_at + self.seconds - now, 3)),
                'seconds_to_hard_stop': max(0, round(self.started_at + self.max_seconds - now, 3)),
                'instruction': ('Cancel owned operations, release session, close experiment, export evidence, '
                                'close and verify lab absence, then report now.' if cleanup else
                                'Finish experimental work before cleanup; reserve time for teardown and report.')}
        if self.stage_budget:
            stage = json.loads(self.stage_budget.read_text())
            result['stage'] = stage
            result['seconds_to_stage_stop'] = max(0, round(stage['stop_at_unix'] - now, 3))
            result['report_margin_seconds'] = 30
        return result

    def bound_wait(self, message):
        if not isinstance(message, dict) or message.get('method') != 'tools/call':
            return message
        params = message.get('params')
        if not isinstance(params, dict) or params.get('name') not in WAIT_TOOLS:
            return message
        arguments = params.get('arguments')
        if not isinstance(arguments, dict):
            return message
        requested = arguments.get('timeout_seconds')
        # All four MCP wait tools accept 1..=120. Invalid timeouts must reach
        # normal MCP validation unchanged, not become valid through clamping.
        if type(requested) is not int or not 1 <= requested <= 120:
            return message
        budget = self.budget()
        if (budget['phase'] == 'cleanup' and params['name'] == 'proofstorm_lab_wait'
                and arguments.get('target_phase') == 'closed'):
            # Ordinary deletion should not burn the remaining model steps in
            # short polls. Keep a reporting margin and the server's 1s minimum
            # if that margin has already been entered; admission is unchanged.
            bound = max(1, min(60, math.floor(budget['seconds_to_hard_stop'] - 30)))
        else:
            remaining = (min(10, budget['seconds_to_hard_stop']) if budget['phase'] == 'cleanup'
                         else budget['seconds_to_cleanup'])
            bound = max(1, math.ceil(remaining))
        if 'seconds_to_stage_stop' in budget:
            bound = min(bound, max(1, math.floor(budget['seconds_to_stage_stop'] - 30)))
        arguments['timeout_seconds'] = min(requested, bound)
        return message

    def decorate(self, reply):
        try:
            budget = self.budget()
        except (OSError, ValueError, TypeError, KeyError):
            budget = {'phase': 'cleanup', 'budget_unavailable': True,
                      'instruction': 'Budget unavailable: stop work and prioritize cleanup.'}
        if isinstance(reply.get('error'), dict):
            data = reply['error'].setdefault('data', {})
            if isinstance(data, dict):
                data['_benchmark_budget'] = budget
            # Some clients surface only error.message, discarding data. Keep the
            # same refusal diagnostic while making both deadlines visible there.
            if self.stage_budget:
                reply['error']['message'] = str(reply['error'].get('message', '')) + (
                    ' [Benchmark budget: ' + json.dumps(budget) + ']')
        result = reply.get('result')
        if isinstance(result, dict):
            # Preserve the tool document so ordinary MCP clients and the
            # evaluator still parse one JSON value, with benchmark metadata.
            for block in result.get('content', []):
                if block.get('type') != 'text':
                    continue
                try:
                    document = json.loads(block.get('text', ''))
                except (ValueError, TypeError):
                    continue
                if isinstance(document, dict):
                    document['_benchmark_budget'] = budget
                    block['text'] = json.dumps(document)
            if isinstance(result.get('structuredContent'), dict):
                result['structuredContent']['_benchmark_budget'] = budget
        return reply


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--events', required=True)
    parser.add_argument('--state', required=True)
    parser.add_argument('--started-at', type=float, required=True)
    parser.add_argument('--max-seconds', type=int, required=True)
    parser.add_argument('--max-steps', type=int, required=True)
    parser.add_argument('--max-context-tokens', type=int, default=0)
    parser.add_argument('--max-processed-tokens', type=int, default=0)
    parser.add_argument('--argument-audit')
    parser.add_argument('--public-help-only', action='store_true')
    parser.add_argument('--stage-budget')
    parser.add_argument('command', nargs=argparse.REMAINDER)
    args = parser.parse_args()
    command = args.command[1:] if args.command[:1] == ['--'] else args.command
    if (not command or args.max_seconds < 1 or args.max_steps < 1
            or args.max_context_tokens < 0 or args.max_processed_tokens < 0):
        parser.error('positive limits and an MCP command are required')
    gate = CleanupGate(args.events, args.state, args.started_at, args.max_seconds, args.max_steps,
                       args.max_context_tokens, args.max_processed_tokens, args.stage_budget)
    child = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                             start_new_session=True, text=True, bufsize=1)
    lock = threading.Lock()
    pending_lock = threading.Lock()
    pending_tools = set()

    def audit(message, boundary):
        if not args.argument_audit:
            return
        record = argument_snapshot(message, boundary)
        if record is not None:
            fd = os.open(args.argument_audit, os.O_APPEND | os.O_CREAT | os.O_WRONLY, 0o600)
            with os.fdopen(fd, 'w') as stream:
                stream.write(json.dumps(record) + '\n')

    def emit(line):
        with lock:
            sys.stdout.write(line)
            sys.stdout.flush()

    def forward():
        for line in child.stdout:
            try:
                reply = json.loads(line)
                with pending_lock:
                    is_tool = reply.get('id') in pending_tools
                    pending_tools.discard(reply.get('id'))
                if is_tool:
                    line = json.dumps(gate.decorate(reply)) + '\n'
            except (ValueError, TypeError, OSError):
                pass  # Never lose an execution receipt to budget annotation.
            emit(line)

    worker = threading.Thread(target=forward, daemon=True)
    worker.start()

    def stop(_signal, _frame):
        raise SystemExit(143)

    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    try:
        for line in sys.stdin:
            try:
                message = json.loads(line)
            except ValueError:
                continue
            audit(message, 'mcp_proxy_received')
            if args.public_help_only and not public_output_allowed(message):
                identity = message.get('params', {}).get('arguments', {}).get('operation_id')
                if args.argument_audit:
                    fd = os.open(args.argument_audit, os.O_APPEND | os.O_CREAT | os.O_WRONLY, 0o600)
                    with os.fdopen(fd, 'w') as stream:
                        stream.write(json.dumps({'boundary': 'public_output_refused', 'at_unix': time.time(),
                            'operation_id_sha256': hashlib.sha256(identity.encode()).hexdigest() if isinstance(identity, str) else None,
                            'code': 'public_output_help_only'}) + '\n')
                if 'id' in message:
                    emit(json.dumps(gate.decorate({'jsonrpc': '2.0', 'id': message['id'], 'error': {
                        'code': -32600, 'message': 'This scenario permits public output only for exact safe help/version argv. Use private output with no fields for native reconciliation; no command was forwarded.',
                        'data': {'code': 'public_output_help_only'}}})) + '\n')
                continue
            try:
                allowed = gate.allows(message)
            except (OSError, TypeError):
                allowed = False  # An unreadable budget must not reopen mutation authority.
            if not allowed:
                if 'id' in message:
                    emit(json.dumps(gate.decorate({'jsonrpc': '2.0', 'id': message['id'], 'error': {
                        'code': -32600, 'message': 'Cleanup phase: cancel or wait for owned operations, release the session, close the experiment, export evidence, close the lab, then report. New experimental work is refused.',
                        'data': {'code': 'cleanup_phase_only'}}})) + '\n')
                continue
            if message.get('method') == 'tools/call':
                message = gate.bound_wait(message)
                if 'id' in message:
                    with pending_lock:
                        pending_tools.add(message['id'])
                line = json.dumps(message) + '\n'
            child.stdin.write(line)
            child.stdin.flush()
            audit(message, 'mcp_proxy_forwarded')
        child.stdin.close()
        child.wait(timeout=5)
        worker.join(timeout=1)
    finally:
        if child.poll() is None:
            os.killpg(child.pid, signal.SIGTERM)
            try:
                child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                os.killpg(child.pid, signal.SIGKILL)
                child.wait()


if __name__ == '__main__':
    main()
