#!/usr/bin/env python3
"""Audit cluster idleness; optionally retire only one run's owned resources."""
import argparse
import json
from pathlib import Path
import subprocess
import time

ROOT = Path(__file__).resolve().parents[1]
KUBECTL = str(ROOT / '.tools/bin/kubectl')
CONTROL = 'proofstorm-system'


def kubectl(*args):
    return subprocess.check_output(
        [KUBECTL, '--context', 'k3d-proofstorm', '--request-timeout=20s', *args], text=True)


def resources(kind):
    return json.loads(kubectl('get', kind, '-A', '-o', 'json'))['items']


def snapshot():
    items = resources('namespaces,proofstormlabs,proofstormlabactions,proofstormcandidatebuilds,jobs,pods,persistentvolumeclaims,persistentvolumes')
    blockers = []
    control_plane = []
    infrastructure_claims = {
        volume['persistentVolumeClaim']['claimName']
        for item in items
        if item['kind'] == 'Pod'
        and item['metadata'].get('namespace') == CONTROL
        and item['metadata'].get('labels', {}).get('app.kubernetes.io/name') == 'proofstormd'
        for volume in item.get('spec', {}).get('volumes', [])
        if 'persistentVolumeClaim' in volume
    }
    infrastructure_storage = []
    for item in items:
        meta = item['metadata']
        kind, name, ns = item['kind'], meta['name'], meta.get('namespace', '')
        labels = meta.get('labels', {})
        if (kind == 'PersistentVolumeClaim' and ns == CONTROL
                and name in infrastructure_claims and item.get('status', {}).get('phase') == 'Bound'):
            infrastructure_storage.append({'namespace': ns, 'name': name, 'phase': 'Bound'})
            continue
        if kind == 'Pod' and ns == CONTROL and labels.get('app.kubernetes.io/name') == 'proofstormd':
            control_plane.append({'pod': name, 'uid': meta.get('uid'),
                                  'containers': [{'name': c['name'], 'image': c.get('image'),
                                                  'image_id': c.get('imageID'), 'ready': c.get('ready')}
                                                 for c in item.get('status', {}).get('containerStatuses', [])]})
        is_lab_ns = kind == 'Namespace' and ('proofstorm.dev/instance' in labels or name.startswith('proofstorm-i'))
        is_control_work = ns == CONTROL and (
            kind.startswith('Proofstorm') or kind in ('Job', 'PersistentVolumeClaim')
            or (kind == 'Pod' and labels.get('app.kubernetes.io/name') != 'proofstormd'))
        is_lab_work = ns.startswith('proofstorm-i')
        claim_ns = item.get('spec', {}).get('claimRef', {}).get('namespace', '')
        if is_lab_ns or is_control_work or is_lab_work or claim_ns.startswith('proofstorm-i'):
            blockers.append({'kind': kind, 'namespace': ns, 'name': name,
                             'phase': item.get('status', {}).get('phase'),
                             'deleting': bool(meta.get('deletionTimestamp'))})
    return {'context': 'k3d-proofstorm', 'verified_idle': not blockers,
            'blockers': blockers, 'control_plane': control_plane,
            'infrastructure_storage': infrastructure_storage, 'checked_at_unix': int(time.time())}


def retire(run, cleanup):
    manifest = json.loads((run / 'manifest.json').read_text())
    workspace = manifest['workspace']
    records = []
    kinds = ['proofstormcandidatebuilds']
    if cleanup:
        kinds.append('proofstormlabs')
    for kind in kinds:
        for item in resources(kind):
            if item.get('spec', {}).get('workspaceId') != workspace:
                continue
            phase = item.get('status', {}).get('phase', '').lower()
            if kind == 'proofstormcandidatebuilds' and not cleanup and phase not in ('succeeded', 'failed', 'cancelled'):
                continue
            meta = item['metadata']
            records.append({'kind': kind, 'name': meta['name'], 'namespace': meta['namespace'],
                            'phase': phase, 'operator_cleanup': cleanup})
            kubectl('delete', kind, meta['name'], '-n', meta['namespace'],
                    '--cascade=foreground', '--wait=false')
    path = run / ('operator-cleanup.json' if cleanup else 'build-retirement.json')
    previous = json.loads(path.read_text()) if path.exists() else []
    path.write_text(json.dumps(previous + records, indent=2) + '\n')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--retire-builds', type=Path)
    parser.add_argument('--cleanup-run', type=Path)
    parser.add_argument('--wait-seconds', type=int, default=0)
    args = parser.parse_args()
    if args.retire_builds:
        retire(args.retire_builds, False)
    if args.cleanup_run:
        retire(args.cleanup_run, True)
    deadline = time.monotonic() + args.wait_seconds
    try:
        while True:
            state = snapshot()
            if state['verified_idle'] or time.monotonic() >= deadline:
                break
            time.sleep(2)
    except (subprocess.CalledProcessError, ValueError) as error:
        state = {'verified_idle': False, 'error': str(error), 'blockers': None}
    args.output.write_text(json.dumps(state, indent=2) + '\n')
    print(json.dumps(state))
    return 0 if state['verified_idle'] else 1


if __name__ == '__main__':
    raise SystemExit(main())
