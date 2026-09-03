import asyncio
import hashlib
import json
import os
import sys

import httpx
from cashu.core.crypto.secp import PrivateKey
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
        "contract": "proofstorm/authentication-conformance/v1",
        "mint": mint,
        "identity_provider": identity,
        "advertised_nut21": False,
        "advertised_nut22": False,
        "invalid_oidc_password_rejected": False,
        "missing_cat_rejected": False,
        "invalid_cat_code": None,
        "missing_bat_rejected": False,
        "invalid_bat_code": None,
        "oidc_login": False,
        "claims_match": False,
        "mint_accepted_cat": False,
        "bat_issued": False,
        "bat_dleq": False,
        "bat_max_code": None,
        "rate_limit_code": None,
        "conformant": False,
        "failure_stage": None,
        "failure_status": None,
        "failure_protocol_code": None,
    }

    def finding(stage, response=None):
        result["failure_stage"] = stage
        if response is not None:
            result["failure_status"] = response.status_code
            result["failure_protocol_code"] = protocol_code(response)
        write_result(result)

    async with httpx.AsyncClient(timeout=30) as client:
        info_response = await client.get(mint_url + "/v1/info")
        if info_response.status_code >= 400:
            finding("mint_info", info_response)
            return
        info = info_response.json()
        nut21 = info.get("nuts", {}).get("21")
        nut22 = info.get("nuts", {}).get("22")
        result["advertised_nut21"] = isinstance(nut21, dict)
        result["advertised_nut22"] = isinstance(nut22, dict)
        if not result["advertised_nut21"] or not result["advertised_nut22"]:
            finding("auth_advertisement")
            return
        if nut21.get("client_id") != "cashu-client" or nut22.get("bat_max_mint") != 3:
            finding("auth_policy")
            return

        discovery_response = await client.get(nut21["openid_discovery"])
        if discovery_response.status_code >= 400:
            finding("oidc_discovery", discovery_response)
            return
        discovery = discovery_response.json()
        rejected = await client.post(
            discovery["token_endpoint"],
            data={
                "grant_type": "password",
                "client_id": "cashu-client",
                "username": os.environ["OIDC_TEST_USERNAME"],
                "password": "not-the-generated-password",
                "scope": "openid",
            },
        )
        result["invalid_oidc_password_rejected"] = rejected.status_code >= 400
        if not result["invalid_oidc_password_rejected"]:
            finding("invalid_oidc_password")
            return

        quote_payload = {"amount": 1, "unit": "sat"}
        missing_bat = await client.post(
            mint_url + "/v1/mint/quote/bolt11", json=quote_payload
        )
        result["missing_bat_rejected"] = missing_bat.status_code >= 400
        if not result["missing_bat_rejected"]:
            finding("missing_bat")
            return
        invalid_bat = await client.post(
            mint_url + "/v1/mint/quote/bolt11",
            json=quote_payload,
            headers={"Blind-auth": "authAinvalid"},
        )
        result["invalid_bat_code"] = protocol_code(invalid_bat)
        if invalid_bat.status_code < 400 or result["invalid_bat_code"] != 81002:
            finding("invalid_bat", invalid_bat)
            return

        missing_cat = await client.post(
            mint_url + "/v1/auth/blind/mint", json={"outputs": []}
        )
        result["missing_cat_rejected"] = missing_cat.status_code >= 400
        if not result["missing_cat_rejected"]:
            finding("missing_cat")
            return
        invalid_cat = await client.post(
            mint_url + "/v1/auth/blind/mint",
            json={"outputs": []},
            headers={"Clear-auth": "not-a-jwt"},
        )
        result["invalid_cat_code"] = protocol_code(invalid_cat)
        if invalid_cat.status_code < 400 or result["invalid_cat_code"] != 80002:
            finding("invalid_cat", invalid_cat)
            return

    wallet_dir = "/tmp/proofstorm-auth-conformance"
    os.makedirs(wallet_dir, mode=0o700, exist_ok=True)
    wallet = await WalletAuth.with_db(
        url=mint_url,
        db=wallet_dir,
        username=os.environ["OIDC_TEST_USERNAME"],
        password=os.environ["OIDC_TEST_PASSWORD"],
        client_id="cashu-client",
    )
    required = await wallet.init_auth_wallet(mint_auth_proofs=False, force_auth=True)
    result["oidc_login"] = bool(required and wallet.oidc_client.access_token)
    if not result["oidc_login"]:
        finding("oidc_login")
        return
    claims = __import__("jwt").decode(
        wallet.oidc_client.access_token, options={"verify_signature": False}
    )
    result["claims_match"] = bool(
        claims.get("sub")
        and claims.get("iss") == discovery.get("issuer")
        and claims.get("azp") == "cashu-client"
        and isinstance(claims.get("iat"), int)
        and isinstance(claims.get("exp"), int)
        and claims["exp"] - claims["iat"] == 600
    )
    if not result["claims_match"]:
        finding("oidc_claims")
        return

    def outputs(count):
        secrets = [hashlib.sha256(os.urandom(32)).hexdigest() for _ in range(count)]
        blinded, _ = wallet._construct_outputs(
            [1] * count,
            secrets,
            [PrivateKey(os.urandom(32)) for _ in range(count)],
        )
        return [entry.model_dump() for entry in blinded]

    async with httpx.AsyncClient(timeout=30) as client:
        headers = {"Clear-auth": wallet.oidc_client.access_token}
        excessive = await client.post(
            mint_url + "/v1/auth/blind/mint",
            json={"outputs": outputs(4)},
            headers=headers,
        )
        result["bat_max_code"] = protocol_code(excessive)
        if excessive.status_code < 400 or result["bat_max_code"] != 81003:
            finding("bat_maximum", excessive)
            return

        accepted = await client.post(
            mint_url + "/v1/auth/blind/mint",
            json={"outputs": outputs(1)},
            headers=headers,
        )
        result["mint_accepted_cat"] = (
            accepted.status_code not in (401, 403)
            and protocol_code(accepted) != 80002
        )
        if accepted.status_code >= 400:
            finding("bat_issuance", accepted)
            return
        signatures = accepted.json().get("signatures", [])
        result["bat_issued"] = len(signatures) == 1
        result["bat_dleq"] = bool(
            result["bat_issued"] and signatures[0].get("dleq")
        )
        if not result["bat_issued"] or not result["bat_dleq"]:
            finding("bat_signature")
            return

        limited = await client.post(
            mint_url + "/v1/auth/blind/mint",
            json={"outputs": outputs(1)},
            headers=headers,
        )
        result["rate_limit_code"] = protocol_code(limited)
        if limited.status_code < 400 or result["rate_limit_code"] != 81004:
            finding("cat_rate_limit", limited)
            return

    result["conformant"] = True
    write_result(result)


try:
    asyncio.run(run())
except Exception:
    write_result(
        {
            "code": "authentication_conformance_failed",
            "stage": "driver",
            "reason": "unexpected_driver_error",
        }
    )
    sys.exit(1)
