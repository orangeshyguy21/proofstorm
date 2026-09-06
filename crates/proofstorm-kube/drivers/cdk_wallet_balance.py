"""Passive CDK CLI 0.18 SQLite observation. Never start the wallet or recover sagas."""

import json
import os
from pathlib import Path
import sqlite3
import sys
import time


def observe(database, wallet, mint, mint_url):
    # mode=ro refuses missing databases. Do not use immutable=1 on a live WAL.
    # The directory must permit SQLite WAL coordination files; wallet records
    # remain read-only and no wallet/SDK recovery code is loaded.
    with sqlite3.connect(Path(database).resolve().as_uri() + "?mode=ro", timeout=3) as connection:
        connection.execute("PRAGMA query_only=ON")
        connection.execute("BEGIN")
        columns = {row[1] for row in connection.execute("PRAGMA table_info(proof)")}
        if not {"mint_url", "unit", "state", "amount"} <= columns:
            raise ValueError("wallet_schema_mismatch")
        rows = connection.execute(
            "SELECT state, amount FROM proof WHERE mint_url = ? AND unit = ?",
            (mint_url, "sat"),
        )
        amounts = dict.fromkeys(("UNSPENT", "RESERVED", "PENDING", "PENDING_SPENT", "SPENT"), 0)
        for state, amount in rows:
            if state not in amounts or type(amount) is not int or amount < 0:
                raise ValueError("wallet_proof_state_invalid")
            amounts[state] += amount
        if sum(amounts.values()) > 2**64 - 1:
            raise ValueError("wallet_balance_overflow")
    # These are wallet-local classifications, not authoritative mint proof states.
    return {
        "wallet": wallet,
        "mint": mint,
        "unit": "sat",
        "balance_sat": amounts["UNSPENT"],
        "reserved_sat": amounts["RESERVED"],
        "pending_sat": amounts["PENDING"],
        "pending_spent_sat": amounts["PENDING_SPENT"],
        "observation_source": "cdk-cli/0.18/sqlite-read-transaction/v1",
        "observed_at_unix": int(time.time()),
    }


if __name__ == "__main__":
    try:
        result = observe(
            os.environ["PROOFSTORM_DATABASE"],
            os.environ["PROOFSTORM_WALLET"],
            os.environ["PROOFSTORM_MINT"],
            os.environ["PROOFSTORM_MINT_URL"],
        )
    except (sqlite3.Error, ValueError, KeyError):
        # Database contents and paths must not leak into ordinary artifacts.
        print(json.dumps({"code": "wallet_orchestration_failed", "stage": "observation", "reason": "database_missing_busy_or_incompatible"}))
        sys.exit(1)
    print(json.dumps(result, separators=(",", ":"), sort_keys=True))
