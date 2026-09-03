# Settlement oracle for the wallet pay job. Runs inside the pinned Nutshell
# wallet image after `cashu pay` and derives the payment outcome from the
# wallet's own melt-quote ledger, never from the CLI exit code. Writes one JSON
# document to stdout: the terminal artifact on success, or a
# wallet_orchestration_failed diagnostic when no verdict can be reached.
import glob
import json
import os
import sqlite3
import sys

PHASES = {"PAID": "paid", "UNPAID": "unpaid", "PENDING": "pending"}


def fail(reason):
    sys.stdout.write(
        json.dumps(
            {
                "code": "wallet_orchestration_failed",
                "stage": "settlement",
                "reason": reason,
            }
        )
    )
    sys.exit(1)


invoice = os.environ["PROOFSTORM_INVOICE"]
wallet = os.environ["PROOFSTORM_WALLET"]
pay_exit_code = int(os.environ["PROOFSTORM_PAY_RC"])
balance_before = int(os.environ["PROOFSTORM_BALANCE_BEFORE"])
balance_after = int(os.environ["PROOFSTORM_BALANCE_AFTER"])
wallet_dir = os.path.join(os.environ["HOME"], ".cashu", wallet)

row = None
for path in sorted(glob.glob(os.path.join(wallet_dir, "*.sqlite3"))):
    try:
        connection = sqlite3.connect("file:%s?mode=ro" % path, uri=True)
        row = connection.execute(
            "SELECT quote, state, amount, fee_reserve, fee_paid"
            " FROM bolt11_melt_quotes WHERE lower(request) = lower(?)"
            " ORDER BY created_time DESC LIMIT 1",
            (invoice,),
        ).fetchone()
        connection.close()
    except sqlite3.Error:
        continue
    if row is not None:
        break

if row is None:
    fail("melt_quote_missing")
melt_quote_id, state, amount, fee_reserve, fee_paid = row
phase = PHASES.get(state)
if phase is None:
    fail("melt_quote_state_unknown")

artifact = {
    "quote_id": os.environ["PROOFSTORM_QUOTE_ID"],
    "wallet": wallet,
    "recipient_wallet": os.environ["PROOFSTORM_RECIPIENT_WALLET"],
    "direction": "pay",
    "phase": phase,
    "amount_sat": int(os.environ["PROOFSTORM_AMOUNT_SAT"]),
    "balance_sat": balance_after,
    "balance_before_sat": balance_before,
    "balance_after_sat": balance_after,
    "melt_quote_id": melt_quote_id,
    "melt_quote_state": state,
    "melt_amount_sat": amount,
    "fee_reserve_sat": fee_reserve,
    "fee_paid_sat": fee_paid,
    "pay_exit_code": pay_exit_code,
}
sys.stdout.write(json.dumps(artifact, separators=(",", ":"), sort_keys=True))
