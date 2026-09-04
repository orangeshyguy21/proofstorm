"""Private Nutshell quote reader and recipient-claim driver.

The driver runs inside the pinned wallet image. It emits only sanitized JSON;
the BOLT11 request is used for exact correlation but is never serialized.
"""

import asyncio
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


def wallet_databases(home=None, wallet=None):
    wallet = wallet or required("PROOFSTORM_WALLET")
    wallet_dir = os.path.join(home or required("HOME"), ".cashu", wallet)
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


def query_with_retry(query, parameters, missing_reason, home=None, wallet=None):
    _, paths = wallet_databases(home, wallet)
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


def receive_row(target_quote_id, home=None, wallet=None, expected_mint=None):
    row = query_with_retry(
        "SELECT quote, mint, state, amount, created_time, paid_time, expiry, request "
        "FROM bolt11_mint_quotes WHERE quote = ? LIMIT 1",
        (target_quote_id,),
        "mint_quote_missing",
        home,
        wallet,
    )
    quote, mint, state, amount, created_time, paid_time, expiry, request = row
    if quote != target_quote_id:
        raise DriverFailure("mint_quote_identity_mismatch")
    expected_mint = expected_mint or os.environ.get("PROOFSTORM_EXPECTED_MINT_URL")
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


def receive_artifact(row, role, wallet=None, mint=None, extra=None):
    artifact = {
        "role": role,
        "direction": "receive",
        "quote_id": row["quote_id"],
        "wallet_id": wallet or required("PROOFSTORM_WALLET"),
        "mint_id": mint or required("PROOFSTORM_MINT"),
        "state": row["state"],
        "amount_sat": row["amount_sat"],
    }
    for source, target in [
        ("created_time", "wallet_created_at_unix"),
        ("paid_time", "wallet_paid_at_unix"),
        ("expiry", "wallet_expires_at_unix"),
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


def melt_row_by_id(target_quote_id):
    row = query_with_retry(
        "SELECT quote, state, amount, fee_reserve, fee_paid "
        "FROM bolt11_melt_quotes WHERE quote = ? LIMIT 1",
        (target_quote_id,),
        "melt_quote_missing",
    )
    if row[0] != target_quote_id:
        raise DriverFailure("melt_quote_identity_mismatch")
    return row


def proof_reservation_snapshot(target_quote_id):
    _, paths = wallet_databases()
    for path in paths:
        connection = None
        try:
            connection = connect_read_only(path, 1)
            reserved = connection.execute(
                "SELECT COUNT(*), COALESCE(SUM(amount), 0) FROM proofs "
                "WHERE melt_id = ? AND reserved",
                (target_quote_id,),
            ).fetchone()
            available = connection.execute(
                "SELECT COALESCE(SUM(amount), 0) FROM proofs WHERE NOT reserved"
            ).fetchone()
            return {
                "reserved_proof_count": int(reserved[0]),
                "reserved_sat": int(reserved[1]),
                "available_balance_sat": int(available[0]),
            }
        except sqlite3.OperationalError as error:
            if "no such table" not in str(error).lower():
                raise DriverFailure("wallet_database_read_failed") from error
        finally:
            if connection is not None:
                connection.close()
    raise DriverFailure("wallet_proof_schema_mismatch")


def authoritative_melt_row(wallet_row):
    """Replace Nutshell 0.20 wallet-local fee accounting with mint facts.

    That wallet release stores `amount + response_fee - change` in its local
    `fee_paid` column. The mint's `melt_quotes` row is the authoritative source
    for the actual Lightning fee and is mounted read-only by wallet-pay jobs.
    """
    quote, wallet_state, wallet_amount, wallet_reserve, wallet_fee = wallet_row
    mint_db_dir = os.environ.get("PROOFSTORM_MINT_DB_DIR")
    if not mint_db_dir:
        return wallet_row
    paths = sorted(
        glob.glob(
            os.path.join(mint_db_dir, "**", "mint.sqlite3"),
            recursive=True,
        )
    )
    if not paths:
        # Non-SQLite mint storage has no local authoritative row. Never expose
        # the known wallet-local compatibility value as an actual network fee.
        return quote, wallet_state, wallet_amount, wallet_reserve, None
    timeout_seconds = bounded_seconds("PROOFSTORM_DB_TIMEOUT_SECONDS", 10, 1, 30)
    retry_seconds = bounded_seconds("PROOFSTORM_DB_RETRY_SECONDS", 0.2, 0.05, 2)
    deadline = time.monotonic() + timeout_seconds
    while True:
        busy = False
        for path in paths:
            connection = None
            try:
                connection = connect_read_only(path, timeout_seconds)
                row = connection.execute(
                    "SELECT quote, state, amount, fee_reserve, fee_paid "
                    "FROM melt_quotes WHERE quote = ? LIMIT 1",
                    (quote,),
                ).fetchone()
                if row is not None:
                    if row[0] != quote:
                        raise DriverFailure("mint_melt_quote_identity_mismatch")
                    return row
            except sqlite3.OperationalError as error:
                message = str(error).lower()
                if any(fragment in message for fragment in BUSY_ERRORS):
                    busy = True
                elif "no such table" not in message and "no such column" not in message:
                    raise DriverFailure("mint_database_read_failed") from error
            finally:
                if connection is not None:
                    connection.close()
        if time.monotonic() >= deadline:
            if busy:
                raise DriverFailure("mint_database_busy")
            raise DriverFailure("mint_melt_quote_missing")
        time.sleep(retry_seconds)


def observe_invoice():
    row = receive_row(invoice_quote_id_from_file())
    if row["state"].upper() != "UNPAID":
        raise DriverFailure("mint_quote_initial_state_unexpected")
    observation = receive_artifact(row, "invoice_receive")
    sys.stdout.write(
        json.dumps(
            {"mint_quote_id": row["quote_id"], "quote_observations": [observation]},
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
    quote, state, amount, fee_reserve, fee_paid = authoritative_melt_row(melt_row())
    artifact = {
        "role": "payment_melt",
        "direction": "pay",
        "quote_id": quote,
        "wallet_id": required("PROOFSTORM_WALLET"),
        "mint_id": required("PROOFSTORM_MINT"),
        "state": str(state),
        "amount_sat": int(amount),
        "fee_reserve_sat": int(fee_reserve),
        "fee_paid_sat": None if fee_paid is None else int(fee_paid),
    }
    sys.stdout.write(json.dumps(artifact, separators=(",", ":"), sort_keys=True))


async def refresh_melt_quote_async():
    target_quote_id = quote_id("PROOFSTORM_MELT_QUOTE_ID")
    expected_mint = required("PROOFSTORM_EXPECTED_MINT_URL")
    wallet_name = required("PROOFSTORM_WALLET")
    before_row = melt_row_by_id(target_quote_id)
    before = proof_reservation_snapshot(target_quote_id)

    from cashu.wallet.wallet import Wallet

    wallet = await Wallet.with_db(
        expected_mint,
        os.path.join(required("HOME"), ".cashu", wallet_name),
        name=wallet_name,
        unit="sat",
    )
    refreshed = await wallet.get_melt_quote(target_quote_id)
    if refreshed is None:
        raise DriverFailure("melt_quote_refresh_missing")

    after = proof_reservation_snapshot(target_quote_id)
    remote_state = str(getattr(refreshed, "state", ""))
    if "." in remote_state:
        remote_state = remote_state.rsplit(".", 1)[-1]
    remote_state = remote_state.upper()
    if remote_state not in ("UNPAID", "PENDING", "PAID"):
        raise DriverFailure("unsupported_wallet_quote_state")

    _, _, amount, fee_reserve, fee_paid = before_row
    observation = {
        "role": "payment_melt",
        "direction": "pay",
        "quote_id": target_quote_id,
        "wallet_id": wallet_name,
        "mint_id": required("PROOFSTORM_MINT"),
        "state": remote_state,
        "amount_sat": int(amount),
        "fee_reserve_sat": int(fee_reserve),
        "fee_paid_sat": None if fee_paid is None else int(fee_paid),
    }
    artifact = {
        "melt_quote_id": target_quote_id,
        "state_before": str(before_row[1]).upper(),
        "state_after": remote_state,
        "reserved_proof_count_before": before["reserved_proof_count"],
        "reserved_proof_count_after": after["reserved_proof_count"],
        "reserved_sat_before": before["reserved_sat"],
        "reserved_sat_after": after["reserved_sat"],
        "available_balance_sat_before": before["available_balance_sat"],
        "available_balance_sat_after": after["available_balance_sat"],
        "proofs_released": (
            before["reserved_proof_count"] > 0
            and after["reserved_proof_count"] == 0
        ),
        "quote_observations": [observation],
    }
    sys.stdout.write(json.dumps(artifact, separators=(",", ":"), sort_keys=True))


def refresh_melt_quote():
    asyncio.run(refresh_melt_quote_async())


def cashu_command(*arguments, mint_url=None, wallet=None):
    mint_url = mint_url or required("PROOFSTORM_EXPECTED_MINT_URL")
    wallet = wallet or required("PROOFSTORM_WALLET")
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
        result = receive_artifact(before, "claim_receive")
        claim_exit_code = 0
        already_issued = True
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
        result = receive_artifact(after, "claim_receive")
        already_issued = False
    artifact = {
        "mint_quote_id": target_quote_id,
        "claim_exit_code": claim_exit_code,
        "already_issued": already_issued,
        "quote_observations": [result],
    }
    if result["state"].upper() not in ("UNPAID", "PAID", "ISSUED"):
        artifact["code"] = "unsupported_wallet_quote_state"
    sys.stdout.write(
        json.dumps(
            artifact,
            separators=(",", ":"),
            sort_keys=True,
        )
    )


def melt_ids():
    _, paths = wallet_databases()
    values = set()
    for path in paths:
        connection = None
        try:
            connection = connect_read_only(path, 1)
            values.update(row[0] for row in connection.execute("SELECT quote FROM bolt11_melt_quotes"))
        except sqlite3.OperationalError as error:
            if "no such table" not in str(error).lower():
                raise DriverFailure("wallet_database_read_failed") from error
        finally:
            if connection is not None:
                connection.close()
    return values


def wallet_balance(home, wallet, mint_url):
    environment = os.environ.copy()
    environment["HOME"] = home
    completed = subprocess.run(
        cashu_command("balance", mint_url=mint_url, wallet=wallet),
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        timeout=30,
        check=False,
        env=environment,
    )
    matches = re.findall(rb"Balance:\s*([0-9]+)", completed.stdout)
    if completed.returncode != 0 or not matches:
        raise DriverFailure("wallet_balance_unavailable")
    return int(matches[-1])


def melt_input_fee(quote, state, mint_url):
    """Derive the exact NUT-02 input fee from the proofs selected for a melt.

    Nutshell preserves the melt quote ID when it moves paid inputs into
    `proofs_used`. Each proof's keyset records its fee in parts per thousand,
    and Nutshell charges ceil(sum(input_fee_ppk) / 1000). This is independent
    evidence; it is never inferred from the observed balance difference.
    """
    _, paths = wallet_databases()
    timeout_seconds = bounded_seconds("PROOFSTORM_DB_TIMEOUT_SECONDS", 10, 1, 30)
    retry_seconds = bounded_seconds("PROOFSTORM_DB_RETRY_SECONDS", 0.2, 0.05, 2)
    deadline = time.monotonic() + timeout_seconds
    query = (
        "SELECT COUNT(*), "
        "COALESCE(SUM(COALESCE((SELECT MAX(k.input_fee_ppk) FROM keysets k "
        "WHERE k.id = p.id AND lower(rtrim(k.mint_url, '/')) = "
        "lower(rtrim(?, '/'))), 0)), 0), "
        "COALESCE(SUM(CASE WHEN (SELECT MAX(k.input_fee_ppk) FROM keysets k "
        "WHERE k.id = p.id AND lower(rtrim(k.mint_url, '/')) = "
        "lower(rtrim(?, '/'))) IS NULL THEN 1 ELSE 0 END), 0) "
        "FROM proofs_used p WHERE p.melt_id = ?"
    )
    while True:
        matches = []
        busy = False
        saw_schema = False
        for path in paths:
            connection = None
            try:
                connection = connect_read_only(path, timeout_seconds)
                row = connection.execute(
                    query, (mint_url, mint_url, quote)
                ).fetchone()
                saw_schema = True
                if row is not None and int(row[0]) > 0:
                    matches.append(tuple(int(value) for value in row))
            except sqlite3.OperationalError as error:
                message = str(error).lower()
                if any(fragment in message for fragment in BUSY_ERRORS):
                    busy = True
                elif "no such table" not in message and "no such column" not in message:
                    raise DriverFailure("wallet_database_read_failed") from error
            except (sqlite3.Error, TypeError, ValueError) as error:
                raise DriverFailure("wallet_database_read_failed") from error
            finally:
                if connection is not None:
                    connection.close()
        if not busy:
            if len(matches) > 1:
                raise DriverFailure("melt_input_proofs_ambiguous")
            if str(state).upper() == "PAID" and len(matches) == 1:
                proof_count, fee_ppk, missing_keysets = matches[0]
                if missing_keysets:
                    raise DriverFailure("melt_input_keyset_missing")
                if proof_count > 10_000 or fee_ppk < 0 or fee_ppk > 100_000_000:
                    raise DriverFailure("melt_input_fee_out_of_bounds")
                return (fee_ppk + 999) // 1000, proof_count
            if str(state).upper() != "PAID":
                if matches:
                    raise DriverFailure("unpaid_melt_spent_proofs_present")
                if saw_schema:
                    return 0, 0
        if time.monotonic() >= deadline:
            if busy:
                raise DriverFailure("wallet_database_busy")
            if not saw_schema:
                raise DriverFailure("wallet_schema_mismatch")
            raise DriverFailure("melt_input_proofs_missing")
        time.sleep(retry_seconds)


def pay_and_claim():
    payer_home = required("HOME")
    payer_wallet = required("PROOFSTORM_WALLET")
    payer_mint = required("PROOFSTORM_MINT")
    payer_mint_url = required("PROOFSTORM_EXPECTED_MINT_URL")
    recipient_home = required("PROOFSTORM_RECIPIENT_HOME")
    recipient_wallet = required("PROOFSTORM_RECIPIENT_WALLET")
    recipient_mint = required("PROOFSTORM_RECIPIENT_MINT")
    recipient_mint_url = required("PROOFSTORM_RECIPIENT_MINT_URL")
    target_quote_id = quote_id("PROOFSTORM_MINT_QUOTE_ID")
    receive = receive_row(
        target_quote_id,
        recipient_home,
        recipient_wallet,
        recipient_mint_url,
    )
    if receive["state"].upper() != "UNPAID":
        code = (
            "mint_quote_not_payable"
            if receive["state"].upper() in ("PAID", "ISSUED")
            else "unsupported_wallet_quote_state"
        )
        sys.stdout.write(
            json.dumps(
                {
                    "code": code,
                    "mint_quote_id": target_quote_id,
                    "quote_observations": [
                        receive_artifact(
                            receive,
                            "payment_receive",
                            recipient_wallet,
                            recipient_mint,
                        )
                    ],
                },
                separators=(",", ":"),
                sort_keys=True,
            )
        )
        return
    invoice = receive["_request"]
    before_ids = melt_ids()
    timeout_seconds = bounded_seconds("PROOFSTORM_PAY_TIMEOUT_SECONDS", 120, 1, 180)
    try:
        completed = subprocess.run(
            cashu_command("pay", invoice, mint_url=payer_mint_url, wallet=payer_wallet),
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            timeout=timeout_seconds,
            check=False,
            env=os.environ.copy(),
        )
        pay_exit_code = completed.returncode
    except subprocess.TimeoutExpired:
        pay_exit_code = 124
    os.environ["PROOFSTORM_INVOICE"] = invoice
    os.environ["PROOFSTORM_MELT_BEFORE_IDS"] = json.dumps(sorted(before_ids))
    quote, state, amount, fee_reserve, fee_paid = authoritative_melt_row(melt_row())
    input_fee_sat, input_proof_count = melt_input_fee(
        quote, state, payer_mint_url
    )
    melt = {
        "role": "payment_melt",
        "direction": "pay",
        "quote_id": quote,
        "wallet_id": payer_wallet,
        "mint_id": payer_mint,
        "state": str(state),
        "amount_sat": int(amount),
        "fee_reserve_sat": int(fee_reserve),
        "fee_paid_sat": None if fee_paid is None else int(fee_paid),
    }
    observations = [melt]
    claim_exit_code = None
    if str(state).upper() == "PAID":
        claim_environment = os.environ.copy()
        claim_environment["HOME"] = recipient_home
        claim_timeout = bounded_seconds("PROOFSTORM_CLAIM_TIMEOUT_SECONDS", 30, 1, 120)
        try:
            claimed = subprocess.run(
                cashu_command(
                    "invoice",
                    str(receive["amount_sat"]),
                    "--id",
                    target_quote_id,
                    mint_url=recipient_mint_url,
                    wallet=recipient_wallet,
                ),
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                timeout=claim_timeout,
                check=False,
                env=claim_environment,
            )
            claim_exit_code = claimed.returncode
        except subprocess.TimeoutExpired:
            claim_exit_code = 124
    receive = receive_row(
        target_quote_id,
        recipient_home,
        recipient_wallet,
        recipient_mint_url,
    )
    observations.append(
        receive_artifact(
            receive,
            "payment_receive",
            recipient_wallet,
            recipient_mint,
        )
    )
    artifact = {
        "mint_quote_id": target_quote_id,
        "melt_quote_id": quote,
        "pay_exit_code": pay_exit_code,
        "claim_exit_code": claim_exit_code,
        "payer_balance_sat": wallet_balance(
            payer_home, payer_wallet, payer_mint_url
        ),
        "input_fee_sat": input_fee_sat,
        "input_proof_count": input_proof_count,
        "recipient_balance_sat": wallet_balance(
            recipient_home, recipient_wallet, recipient_mint_url
        ),
        "quote_observations": observations,
    }
    if str(state).upper() == "PAID" and receive["state"].upper() != "ISSUED":
        artifact["code"] = "payment_paid_claim_unverified"
    elif str(state).upper() not in ("UNPAID", "PENDING", "PAID"):
        artifact["code"] = "unsupported_wallet_quote_state"
    elif receive["state"].upper() not in ("UNPAID", "PAID", "ISSUED"):
        artifact["code"] = "unsupported_wallet_quote_state"
    sys.stdout.write(json.dumps(artifact, separators=(",", ":"), sort_keys=True))


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
    elif mode == "refresh-melt":
        refresh_melt_quote()
    elif mode == "pay-and-claim":
        pay_and_claim()
    else:
        raise DriverFailure("quote_driver_mode_invalid")


try:
    main()
except DriverFailure as error:
    fail(error.reason)
except (OSError, ValueError, TypeError):
    fail("quote_driver_failed")
