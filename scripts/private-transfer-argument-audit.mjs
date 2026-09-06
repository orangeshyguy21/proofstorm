// Run-local observation only. Never persist native argv/script or arbitrary strings.
import { appendFileSync } from 'node:fs';
import { createHash } from 'node:crypto';

function snapshot(args) {
  const transfer = args?.transfer;
  const fields = {};
  for (const key of ['transferMethod', 'component', 'destinationComponent', 'maximumBytes', 'reference']) {
    const present = transfer && typeof transfer === 'object' && Object.hasOwn(transfer, key);
    const value = present ? transfer[key] : undefined;
    const allowed = key === 'transferMethod' ? ['prepare', 'status', 'deliver', 'release'].includes(value)
      : ['component', 'destinationComponent'].includes(key) ? ['wallet-a', 'wallet-b'].includes(value)
      : key === 'maximumBytes' ? Number.isInteger(value) && value >= 0 && value <= 1048576
      : typeof value === 'string' && /^payload-[0-9a-f]{64}$/.test(value);
    fields[key] = !present ? { state: 'missing' } : value === null ? { state: 'null' }
      : allowed ? { state: 'allowed', value } : { state: 'withheld' };
  }
  return {
    operation_id_sha256: typeof args?.operation_id === 'string'
      ? createHash('sha256').update(args.operation_id).digest('hex') : null,
    fields,
  };
}

export default async function argumentAudit() {
  return {
    'tool.execute.before': async (input, output) => {
      if (!input.tool.endsWith('proofstorm_private_transfer')) return;
      const file = process.env.PROOFSTORM_ARGUMENT_AUDIT;
      if (!file) return;
      appendFileSync(file, JSON.stringify({ boundary: 'opencode_tool_execute_before',
        at_unix: Date.now() / 1000, ...snapshot(output.args) }) + '\n', { mode: 0o600 });
    },
  };
}
