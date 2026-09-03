"""Private Nutshell quote reader and recipient-claim driver.

The driver runs inside the pinned wallet image. It emits only sanitized JSON;
the BOLT11 request is used for exact correlation but is never serialized.
"""

import glob
import json
import os
import re
import sqlite3
import subprocess
import sys
import time


QUOTE_ID = re.compile(r"^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$")
INVOICE_QUOTE_ID = re.compile(r"--id\s+([a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?)")
BUSY_ERRORS = ("database is locked", "database is busy", "database table is locked")


class DriverFailure(Exception):
    def __init__(self, reason):
        super().__init__(reason)
        self.reason = reason


def fail(reason):
    sys.stdout.write(
        json.dumps(
            {
                "code": "wallet_orchestration_failed",
                "stage": "quote",
                "reason": reason,
            },
            separators=(",", ":"),
            sort_keys=True,
        )
    )
    sys.exit(1)


def required(name):
    value = os.environ.get(name)
    if not value:
        raise DriverFailure("%s_missing" % name.lower())
    return value


def bounded_seconds(name, default, minimum, maximum):
    raw = os.environ.get(name, str(default))
    try:
        value = float(raw)
    except ValueError as error:
        raise DriverFailure("%s_invalid" % name.lower()) from error
    if value < minimum or value > maximum:
        raise DriverFailure("%s_invalid" % name.lower())
    return value


def quote_id(name):
    value = required(name)
    if not QUOTE_ID.fullmatch(value):
        raise DriverFailure("%s_invalid" % name.lower())
    return value


def wallet_databases():
    wallet = required("PROOFSTORM_WALLET")
    wallet_dir = os.path.join(required("HOME"), ".cashu", wallet)
    paths = sorted(glob.glob(os.path.join(wallet_dir, "*.sqlite3")))
    if not paths:
        raise DriverFailure("wallet_database_missing")
    return wallet, paths


def connect_read_only(path, timeout_seconds):
    return sqlite3.connect(
        "file:%s?mode=ro" % path,
        uri=True,
        timeout=min(timeout_seconds, 1.0),
    )


def query_with_retry(query, parameters, missing_reason):
    _, paths = wallet_databases()
    timeout_seconds = bounded_seconds("PROOFSTORM_DB_TIMEOUT_SECONDS", 10, 1, 30)
    retry_seconds = bounded_seconds("PROOFSTORM_DB_RETRY_SECONDS", 0.2, 0.05, 2)
    deadline = time.monotonic() + timeout_seconds
    saw_schema = False
    while True:
        busy = False
        for path in paths:
            connection = None
            try:
                connection = connect_read_only(path, timeout_seconds)
                row = connection.execute(query, parameters).fetchone()
                saw_schema = True
                if row is not None:
                    return row
            except sqlite3.OperationalError as error:
                message = str(error).lower()
                if any(fragment in message for fragment in BUSY_ERRORS):
                    busy = True
                elif "no such table" not in message and "no such column" not in message:
                    raise DriverFailure("wallet_database_read_failed") from error
            except sqlite3.Error as error:
                raise DriverFailure("wallet_database_read_failed") from error
            finally:
                if connection is not None:
                    connection.close()
        if time.monotonic() >= deadline:
            if busy:
                raise DriverFailure("wallet_database_busy")
            if not saw_schema:
                raise DriverFailure("wallet_schema_mismatch")
            raise DriverFailure(missing_reason)
        time.sleep(retry_seconds)


def normalized_mint(value):
    return value.rstrip("/").lower()


def receive_row(target_quote_id):
    row = query_with_retry(
        "SELECT quote, mint, state, amount, created_time, paid_time, expiry, request "
        "FROM bolt11_mint_quotes WHERE quote = ? LIMIT 1",
        (target_quote_id,),
        "mint_quote_missing",
    )
    quote, mint, state, amount, created_time, paid_time, expiry, request = row
    if quote != target_quote_id:
        raise DriverFailure("mint_quote_identity_mismatch")
    expected_mint = os.environ.get("PROOFSTORM_EXPECTED_MINT_URL")
    if expected_mint and normalized_mint(mint) != normalized_mint(expected_mint):
        raise DriverFailure("mint_quote_mint_mismatch")
    try:
        amount_sat = int(amount)
    except (TypeError, ValueError) as error:
        raise DriverFailure("mint_quote_amount_invalid") from error
    if amount_sat < 1 or amount_sat > 500_000:
        raise DriverFailure("mint_quote_amount_out_of_bounds")
    if not request:
        raise DriverFailure("mint_quote_request_missing")
    return {
        "quote_id": quote,
        "state": str(state),
        "amount_sat": amount_sat,
        "created_time": created_time,
        "paid_time": paid_time,
        "expiry": expiry,
        "_request": request,
    }


def receive_artifact(row, role, extra=None):
    artifact = {
        "role": role,
        "direction": "receive",
        "quote_id": row["quote_id"],
        "wallet": required("PROOFSTORM_WALLET"),
        "mint": required("PROOFSTORM_MINT"),
        "state": row["state"],
        "amount_sat": row["amount_sat"],
        "request_present": True,
    }
    for source, target in [
        ("created_time", "wallet_created_time"),
        ("paid_time", "wallet_paid_time"),
        ("expiry", "wallet_expiry"),
    ]:
        if row[source] is not None:
            artifact[target] = row[source]
    if extra:
        artifact.update(extra)
    return artifact


def invoice_quote_id_from_file():
    path = required("PROOFSTORM_INVOICE_OUTPUT_PATH")
    with open(path, "r", encoding="utf-8", errors="replace") as output:
        text = output.read(64 * 1024)
    matches = sorted(set(INVOICE_QUOTE_ID.findall(text)))
    if len(matches) != 1:
        raise DriverFailure("mint_quote_id_not_observed")
    return matches[0]


def parse_before_ids():
    try:
        values = json.loads(os.environ.get("PROOFSTORM_MELT_BEFORE_IDS", "[]"))
    except json.JSONDecodeError as error:
        raise DriverFailure("melt_before_ids_invalid") from error
    if not isinstance(values, list) or any(
        not isinstance(value, str) or not QUOTE_ID.fullmatch(value) for value in values
    ):
        raise DriverFailure("melt_before_ids_invalid")
    return set(values)


def melt_row():
    invoice = required("PROOFSTORM_INVOICE")
    before_ids = parse_before_ids()
    _, paths = wallet_databases()
    timeout_seconds = bounded_seconds("PROOFSTORM_DB_TIMEOUT_SECONDS", 10, 1, 30)
    retry_seconds = bounded_seconds("PROOFSTORM_DB_RETRY_SECONDS", 0.2, 0.05, 2)
    deadline = time.monotonic() + timeout_seconds
    query = (
        "SELECT quote, state, amount, fee_reserve, fee_paid "
        "FROM bolt11_melt_quotes WHERE lower(request) = lower(?) "
        "ORDER BY created_time DESC"
    )
    while True:
        rows = []
        busy = False
        saw_schema = False
        for path in paths:
            connection = None
            try:
                connection = connect_read_only(path, timeout_seconds)
                rows.extend(connection.execute(query, (invoice,)).fetchall())
                saw_schema = True
            except sqlite3.OperationalError as error:
                message = str(error).lower()
                if any(fragment in message for fragment in BUSY_ERRORS):
                    busy = True
                elif "no such table" not in message and "no such column" not in message:
                    raise DriverFailure("wallet_database_read_failed") from error
            finally:
                if connection is not None:
                    connection.close()
        new_rows = [row for row in rows if row[0] not in before_ids]
        if len(new_rows) == 1:
            return new_rows[0]
        if len(new_rows) > 1:
            raise DriverFailure("melt_quote_ambiguous")
        if time.monotonic() >= deadline:
            if busy:
                raise DriverFailure("wallet_database_busy")
            if not saw_schema:
                raise DriverFailure("wallet_schema_mismatch")
            raise DriverFailure("melt_quote_missing")
        time.sleep(retry_seconds)


def observe_invoice():
    row = receive_row(invoice_quote_id_from_file())
    sys.stdout.write(
        json.dumps(
            receive_artifact(row, "invoice_receive"),
            separators=(",", ":"),
            sort_keys=True,
        )
    )


def observe_receive():
    row = receive_row(quote_id("PROOFSTORM_MINT_QUOTE_ID"))
    sys.stdout.write(
        json.dumps(
            receive_artifact(row, required("PROOFSTORM_OBSERVATION_ROLE")),
            separators=(",", ":"),
            sort_keys=True,
        )
    )


def observe_melt():
    quote, state, amount, fee_reserve, fee_paid = melt_row()
    artifact = {
        "role": "payment_melt",
        "direction": "pay",
        "quote_id": quote,
        "wallet": required("PROOFSTORM_WALLET"),
        "mint": required("PROOFSTORM_MINT"),
        "state": str(state),
        "amount_sat": int(amount),
        "fee_reserve_sat": int(fee_reserve),
        "fee_paid_sat": None if fee_paid is None else int(fee_paid),
    }
    sys.stdout.write(json.dumps(artifact, separators=(",", ":"), sort_keys=True))


def cashu_command(*arguments):
    mint_url = required("PROOFSTORM_EXPECTED_MINT_URL")
    wallet = required("PROOFSTORM_WALLET")
    return [
        sys.executable,
        "-c",
        "from cashu.wallet.cli.cli import cli; cli()",
        "-h",
        mint_url,
        "-u",
        "sat",
        "-w",
        wallet,
        "-t",
        "-y",
        *arguments,
    ]


def claim_receive():
    target_quote_id = quote_id("PROOFSTORM_MINT_QUOTE_ID")
    before = receive_row(target_quote_id)
    if before["state"] == "ISSUED":
        result = receive_artifact(
            before,
            "claim_receive",
            {"claim_exit_code": 0, "already_issued": True},
        )
    else:
        timeout_seconds = bounded_seconds("PROOFSTORM_CLAIM_TIMEOUT_SECONDS", 30, 1, 120)
        try:
            completed = subprocess.run(
                cashu_command(
                    "invoice",
                    str(before["amount_sat"]),
                    "--id",
                    target_quote_id,
                ),
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                timeout=timeout_seconds,
                check=False,
                env=os.environ.copy(),
            )
            claim_exit_code = completed.returncode
        except subprocess.TimeoutExpired:
            claim_exit_code = 124
        after = receive_row(target_quote_id)
        result = receive_artifact(
            after,
            "claim_receive",
            {"claim_exit_code": claim_exit_code, "already_issued": False},
        )
    sys.stdout.write(json.dumps(result, separators=(",", ":"), sort_keys=True))


def main():
    mode = required("PROOFSTORM_QUOTE_DRIVER_MODE")
    if mode == "observe-invoice":
        observe_invoice()
    elif mode == "observe-receive":
        observe_receive()
    elif mode == "observe-melt":
        observe_melt()
    elif mode == "claim-receive":
        claim_receive()
    else:
        raise DriverFailure("quote_driver_mode_invalid")


try:
    main()
except DriverFailure as error:
    fail(error.reason)
except (OSError, ValueError, TypeError):
    fail("quote_driver_failed")
