# Structured native invoice relay

Status 2026-09-05: implemented; deterministic checkpoint passed and the focused
agent execution was valid with the relay target held. Agent reporting review
still failed. Earlier failed benchmark scores remain unchanged.

Native execution supports two explicit invoice output modes. Both keep raw
stdout/stderr in the existing private capture and return a small validated
`selected_output`. They do not create or pay invoices themselves.

| Native producer | Output mode | Accepted response |
| --- | --- | --- |
| `cocod receive bolt11 <sat>` | `bolt11` | One complete BOLT11 string with optional surrounding whitespace |
| `lncli … addinvoice --amt=<sat>` | `lnd_invoice` | One JSON object with string `payment_request` and lowercase hexadecimal `r_hash` |

Do not set `fields` for these modes. LND responses may contain other members;
these remain private. Duplicate selected members, multiple JSON documents,
missing/wrong-type fields, arbitrary text, checksum/signature errors and
invoice/hash disagreement fail closed. The parser uses the pinned
[LDK BOLT11 implementation](https://docs.rs/lightning-invoice/0.34.1/lightning_invoice/struct.Bolt11Invoice.html).
The cocod binding follows the exact upstream CLI's invoice-only string output,
not a substring search or a generic JSON `output` selector that could also
contain spendable Cashu notes.

On success, the receipt contains:

```json
{
  "exit_code": 0,
  "projection_succeeded": true,
  "stdout": "",
  "stderr": "",
  "selected_output": {
    "payment_request": "<validated BOLT11>",
    "payment_hash": "<64 lowercase hex characters derived from the invoice>",
    "amount_msat": 700000,
    "currency": "bcrt",
    "expires_at_unix": 1788745484
  }
}
```

The amount is null for an amountless invoice; it is never invented as zero.
Currency is the BOLT11 currency prefix. These fields describe the invoice,
not recipient settlement. Expiry is an absolute invoice timestamp, not an
assertion that it is still payable when another command runs.

The caller must check successful native exit, `projection_succeeded`, complete
streams and verified cleanup, then verify expected amount, currency and expiry
before submitting a separate native payment. For LND, use `payinvoice --force
--json` with existing `json_fields` selecting `status,value_sat`. Observe the
recipient independently with `lookupinvoice --rhash <payment_hash>` selecting
`state,settled`, and observe wallet balances. A parsed invoice alone proves none
of those payment outcomes.

Invoice extraction is withheld when the native producer exits nonzero, times
out, is cancelled, lacks verified cleanup, or has incomplete/truncated capture.
`projection_error` is a fixed diagnostic and never includes raw parser data.
The native exit/signal remains intact and is separate from extraction success.
Do not repeat a state-changing producer automatically after extraction failure;
reconcile the original operation first.

Invoices are limited to 4,096 bytes so the validated receipt fits existing
journal budgets. The parser accepts at most 64 KiB of response data; the current
supervisor's stricter 16 KiB per-stream retention also applies, and truncation
fails extraction. These bounds intentionally reject some larger valid invoices.
This mode deliberately exposes the invoice, including any encoded description or
routing metadata; it is not a general secret scrubber.

## Checkpoints

Core fixtures cover both native formats, exact hash/amount/currency extraction,
private extra fields, duplicate fields, trailing documents, malformed invoices,
hash mismatch and size bounds. Linux supervisor tests additionally cover private
stdout/stderr, native exit preservation and suppression after failed/truncated
capture. The existing `e2e-cocod-wallet` gate now relays validated invoices through
normal MCP execution receipts and native payer arguments, with no host invoice
copying or raw LND response reads. It checks invoice amount/network/expiry before
payment and independently observes recipient settlement and wallet balances.

`invoice-relay-01-20260905` passed real 5,000-sat funding, a 700-sat payment,
restart/unlock and another 300-sat payment, ending at 4,000 sats while the second
wallet remained at zero. Three invoice projections succeeded with empty ordinary
streams. All 31 native receipts had complete, untruncated captures and verified
cleanup; two nonzero exits were the existing deliberate duplicate-owner and
no-autostart probes. Normal lab teardown and an independent cluster audit
verified absence without operator rescue. Funding and recipient invoice relay
used normal execution receipts and separate native payer arguments.

Validation: 219 workspace Rust tests, six isolated Linux supervisor tests,
strict Clippy, generated contracts, formatting, MCP doctor and unchanged tool
discovery budgets passed. Source snapshot, logs, receipt audit and build pins
are retained under `dev/wallet-integration-runs/invoice-relay-01-20260905/`.
Controller digest is
`sha256:da2dc871bc2a6ad53932645e411aa43967405b5640e30c72acb66034c1ad1b49`;
runner digest is
`sha256:0eb7342a3d18d776ecca4b18f8c0602fbd757e8293bbf55524123f0f26cf32fc`.
The cocod source and wallet image are unchanged. This remains an uncommitted
development build on the local arm64 cluster.

The single focused agent run `cocod-structured-01-20260905` then verified the
5,000→4,300-sat flow using both structured modes, with no grep/custom extraction,
host invoice copies or payment retries. Independent review matched both exact
producer invoices to their native payer arguments, checked amounts/currency and
expiry at submission, and matched the recipient's settlement lookup to the
validated payment hash. The agent observed normal closure and independent audits
verified idle. It used 32 model steps and about 337.4 seconds within unchanged
600-second/50-step limits.

Its final report incorrectly counted 11 native plus 11 typed actions and
attributed an exit code to the typed restart. Actual evidence verifies **12
native executions plus four typed actions**; all native receipts exited zero
with complete, untruncated streams and verified cleanup. The retained evaluator
score is execution **valid**, target **held**, reporting evidence **insufficient**,
proficiency **failed**. The coordinator's independent correction is retained in
`dev/agent-usability-runs/cocod-structured-01-20260905/coordinator-decision.json`.
No payments were repeated to correct report counts. This is assisted native
execution on a preplanned lab, not autonomous network design or a broad wallet
compatibility claim.
See the [fuzzer's structured-relay review](cocod-structured-invoice-2026-09-05.md)
for the original score and evidence references.

## Private ecash remains a separate next checkpoint

This is intentional public relay of small validated Lightning invoices. It does
not add a token store, inbox or private payload reference. Cashu notes must use
the [planned private payload exchange](wallet-expansion-architecture.md): reserve
capacity before send, capture notes privately, authorize destination-bound
delivery, consume bytes through native wallet input, separate delivery from
redemption and reconcile ambiguous outcomes. Neither mode accepts Cashu tokens.
Do not use a BOLT11 projection or public command argument to relay bearer ecash.
