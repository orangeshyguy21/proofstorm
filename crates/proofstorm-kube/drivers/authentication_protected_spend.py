import asyncio
import json
import os
import sys

import httpx
from cashu.core.base import AuthProof
from cashu.wallet.auth.auth import WalletAuth


RESULT_PATH = "/dev/termination-log"


def write_result(result):
    with open(RESULT_PATH, "w", encoding="utf-8") as handle:
        json.dump(result, handle, separators=(",", ":"), sort_keys=True)


def protocol_code(response):
    try:
        value = response.json().get("code")
        return value if isinstance(value, int) else None
    except Exception:
        return None


async def run():
    mint = os.environ["PROOFSTORM_MINT"]
    identity = os.environ["PROOFSTORM_IDENTITY_PROVIDER"]
    mint_url = os.environ["PROOFSTORM_MINT_URL"]
    result = {
        "contract": "proofstorm/authentication-protected-spend-private/v1",
        "mint": mint,
        "identity_provider": identity,
        "bat_count": 0,
        "bat_dleq": False,
        "protected_request": False,
        "conformant": False,
        "failure_stage": None,
        "failure_status": None,
        "failure_protocol_code": None,
        "spent_bat": None,
    }

    def finding(stage, response=None):
        result["failure_stage"] = stage
        if response is not None:
            result["failure_status"] = response.status_code
            result["failure_protocol_code"] = protocol_code(response)
        write_result(result)

    wallet_dir = "/tmp/proofstorm-auth-protected-spend"
    os.makedirs(wallet_dir, mode=0o700, exist_ok=True)
    wallet = await WalletAuth.with_db(
        url=mint_url,
        db=wallet_dir,
        username=os.environ["OIDC_TEST_USERNAME"],
        password=os.environ["OIDC_TEST_PASSWORD"],
        client_id="cashu-client",
    )
    required = await wallet.init_auth_wallet(mint_auth_proofs=True, force_auth=True)
    if not required:
        finding("oidc_login")
        return
    result["bat_count"] = len(wallet.proofs)
    if result["bat_count"] != 3:
        finding("bat_issuance")
        return
    result["bat_dleq"] = all(proof.dleq is not None for proof in wallet.proofs)
    if not result["bat_dleq"]:
        finding("bat_signature")
        return

    spent_bat = AuthProof.from_proof(wallet.proofs[0]).to_base64()
    async with httpx.AsyncClient(timeout=30) as client:
        protected = await client.post(
            mint_url + "/v1/mint/quote/bolt11",
            json={"amount": 1, "unit": "sat"},
            headers={"Blind-auth": spent_bat},
        )
    result["protected_request"] = bool(
        protected.status_code < 400 and protected.json().get("quote")
    )
    if not result["protected_request"]:
        finding("protected_request", protected)
        return

    result["spent_bat"] = spent_bat
    result["conformant"] = True
    write_result(result)


try:
    asyncio.run(run())
except Exception:
    write_result(
        {
            "code": "authentication_protected_spend_failed",
            "stage": "driver",
            "reason": "unexpected_driver_error",
        }
    )
    sys.exit(1)
