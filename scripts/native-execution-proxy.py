#!/usr/bin/env python3
"""Benchmark-only MCP admission gate. Cleanup is latched and survives reconnects."""
import argparse
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
proofstorm_lease_release proofstorm_experiment_close proofstorm_experiment_read
proofstorm_candidate_cancel proofstorm_candidate_read proofstorm_candidate_list
proofstorm_candidate_wait'''.split())
WAIT_TOOLS = frozenset('''proofstorm_lab_wait
proofstorm_operation_wait proofstorm_operation_wait_many proofstorm_candidate_wait'''.split())


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
                 max_context_tokens=0, max_processed_tokens=0):
        self.events = Path(events)
        self.state = Path(state)
        self.started_at = started_at
        self.seconds = max(1, max_seconds * 80 // 100)
        self.steps = max(1, max_steps * 80 // 100)
        self.max_seconds = max_seconds
        self.max_context_tokens = max_context_tokens
        self.max_processed_tokens = max_processed_tokens

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
        return not self.cleanup() or message.get('params', {}).get('name') in CLEANUP_TOOLS

    def budget(self, now=None):
        now = time.time() if now is None else now
        cleanup = self.cleanup(now)
        return {'phase': 'cleanup' if cleanup else 'work',
                'observed_usage': read_usage(self.events),
                'max_context_tokens': self.max_context_tokens or None,
                'max_processed_tokens': self.max_processed_tokens or None,
                'now_unix': round(now, 3),
                'cleanup_at_unix': self.started_at + self.seconds,
                'hard_stop_at_unix': self.started_at + self.max_seconds,
                'seconds_to_cleanup': max(0, round(self.started_at + self.seconds - now, 3)),
                'seconds_to_hard_stop': max(0, round(self.started_at + self.max_seconds - now, 3)),
                'instruction': ('Cancel owned operations, release lease, close experiment, export evidence, '
                                'close and verify lab absence, then report now.' if cleanup else
                                'Finish experimental work before cleanup; reserve time for teardown and report.')}

    def bound_wait(self, message):
        if message.get('params', {}).get('name') not in WAIT_TOOLS:
            return message
        budget = self.budget()
        remaining = (min(10, budget['seconds_to_hard_stop']) if budget['phase'] == 'cleanup'
                     else budget['seconds_to_cleanup'])
        arguments = message.get('params', {}).get('arguments', {})
        requested = arguments.get('timeout_seconds')
        # Shorten only valid observation waits. Never alter execution deadlines
        # or repair invalid requests on behalf of the model.
        if type(requested) is int and requested >= 1:
            arguments['timeout_seconds'] = min(requested, max(1, math.ceil(remaining)))
        return message

    def decorate(self, reply):
        budget = self.budget()
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
    parser.add_argument('command', nargs=argparse.REMAINDER)
    args = parser.parse_args()
    command = args.command[1:] if args.command[:1] == ['--'] else args.command
    if (not command or args.max_seconds < 1 or args.max_steps < 1
            or args.max_context_tokens < 0 or args.max_processed_tokens < 0):
        parser.error('positive limits and an MCP command are required')
    gate = CleanupGate(args.events, args.state, args.started_at, args.max_seconds, args.max_steps,
                       args.max_context_tokens, args.max_processed_tokens)
    child = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                             start_new_session=True, text=True, bufsize=1)
    lock = threading.Lock()
    pending_lock = threading.Lock()
    pending_tools = set()

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
            try:
                allowed = gate.allows(message)
            except (OSError, TypeError):
                allowed = False  # An unreadable budget must not reopen mutation authority.
            if not allowed:
                if 'id' in message:
                    emit(json.dumps({'jsonrpc': '2.0', 'id': message['id'], 'error': {
                        'code': -32600, 'message': 'Cleanup phase: cancel or wait for owned operations, release the lease, close the experiment, export evidence, close the lab, then report. New experimental work is refused.',
                        'data': {'code': 'cleanup_phase_only'}}}) + '\n')
                continue
            if message.get('method') == 'tools/call':
                message = gate.bound_wait(message)
                if 'id' in message:
                    with pending_lock:
                        pending_tools.add(message['id'])
                line = json.dumps(message) + '\n'
            child.stdin.write(line)
            child.stdin.flush()
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
