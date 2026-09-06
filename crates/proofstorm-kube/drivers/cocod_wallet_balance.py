"""Passive projection of Coco 44e5101c's proof repository. Never load a wallet SDK."""
import json
import os
from pathlib import Path
import sqlite3
import sys
import time


def observe(database, wallet, mint, mint_url):
    with sqlite3.connect(Path(database).resolve().as_uri() + "?mode=ro", timeout=3) as db:
        db.execute("PRAGMA query_only=ON")
        db.execute("BEGIN")
        columns = {row[1] for row in db.execute("PRAGMA table_info(coco_cashu_proofs)")}
        if not {"mintUrl", "unit", "state", "amount", "usedByOperationId"} <= columns:
            raise ValueError("wallet_schema_mismatch")
        migrations = {row[0] for row in db.execute("SELECT id FROM coco_cashu_migrations")}
        latest = "038_keypair_derivation_allocations"
        if latest not in migrations or any(value > latest for value in migrations):
            raise ValueError("wallet_schema_mismatch")
        if not db.execute("SELECT 1 FROM coco_cashu_mints WHERE mintUrl=?", (mint_url,)).fetchone():
            raise ValueError("mint_not_registered")
        amounts = dict.fromkeys(("spendable", "reserved", "inflight", "spent"), 0)
        for state, raw, owner in db.execute(
            "SELECT state, amount, usedByOperationId FROM coco_cashu_proofs WHERE mintUrl=? AND unit='sat'",
            (mint_url,),
        ):
            if state not in ("ready", "inflight", "spent") or not isinstance(raw, str):
                raise ValueError("wallet_proof_state_invalid")
            if not raw.isascii() or not raw.isdecimal() or str(int(raw)) != raw:
                raise ValueError("wallet_amount_invalid")
            amount = int(raw)
            category = ("reserved" if owner else "spendable") if state == "ready" else state
            amounts[category] += amount
        if sum(amounts.values()) > 2**64 - 1:
            raise ValueError("wallet_balance_overflow")
    return {
        "wallet": wallet, "mint": mint, "unit": "sat",
        "balance_sat": amounts["spendable"], "reserved_sat": amounts["reserved"],
        "inflight_sat": amounts["inflight"],
        "total_ready_sat": amounts["spendable"] + amounts["reserved"],
        "observation_source": "cocod/44e5101c/sqlite-read-transaction/v1",
        "observed_at_unix": int(time.time()),
    }


if __name__ == "__main__":
    try:
        result = observe(os.environ["PROOFSTORM_DATABASE"], os.environ["PROOFSTORM_WALLET"],
                         os.environ["PROOFSTORM_MINT"], os.environ["PROOFSTORM_MINT_URL"])
    except (sqlite3.Error, ValueError, KeyError, TypeError):
        print(json.dumps({"code":"wallet_orchestration_failed", "stage":"observation",
                          "reason":"database_missing_busy_or_incompatible"}))
        sys.exit(1)
    print(json.dumps(result, separators=(",", ":"), sort_keys=True))
