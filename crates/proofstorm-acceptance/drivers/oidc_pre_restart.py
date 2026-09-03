import asyncio, hashlib, json, os, sys
import httpx
from cashu.core.crypto.secp import PrivateKey
from cashu.wallet.auth.auth import WalletAuth

payload = json.load(sys.stdin)
mint_url = "http://127.0.0.1:3338"

def expect_code(response, code, label):
    try:
        body = response.json()
    except Exception:
        body = {"body": response.text}
    if response.status_code < 400 or body.get("code") != code:
        raise RuntimeError(f"{label}: expected code {code}, got {response.status_code} {body}")

async def main():
    async with httpx.AsyncClient(timeout=30) as client:
        info_response = await client.get(mint_url + "/v1/info")
        info_response.raise_for_status()
        info = info_response.json()
        nut21 = info["nuts"]["21"]
        nut22 = info["nuts"]["22"]
        if nut21["client_id"] != "cashu-client" or nut22["bat_max_mint"] != 3:
            raise RuntimeError(f"unexpected auth advertisement: {nut21} {nut22}")
        discovery = (await client.get(nut21["openid_discovery"])).json()
        rejected = await client.post(discovery["token_endpoint"], data={
            "grant_type": "password", "client_id": "cashu-client",
            "username": payload["username"], "password": "not-the-generated-password",
            "scope": "openid",
        })
        if rejected.status_code < 400:
            raise RuntimeError("Keycloak accepted an invalid password")
        quote_payload = {"amount": 1, "unit": "sat"}
        missing_blind = await client.post(mint_url + "/v1/mint/quote/bolt11", json=quote_payload)
        if missing_blind.status_code < 400:
            raise RuntimeError("protected quote accepted a missing BAT")
        invalid_blind = await client.post(
            mint_url + "/v1/mint/quote/bolt11", json=quote_payload,
            headers={"Blind-auth": "authAinvalid"},
        )
        expect_code(invalid_blind, 81002, "invalid BAT")
        missing_clear = await client.post(mint_url + "/v1/auth/blind/mint", json={"outputs": []})
        if missing_clear.status_code < 400:
            raise RuntimeError("blind mint accepted a missing CAT")
        invalid_clear = await client.post(
            mint_url + "/v1/auth/blind/mint", json={"outputs": []},
            headers={"Clear-auth": "not-a-jwt"},
        )
        expect_code(invalid_clear, 80002, "invalid CAT")

    wallet_dir = "/tmp/proofstorm-nutshell-auth-pre"
    os.makedirs(wallet_dir, mode=0o700, exist_ok=True)
    wallet = await WalletAuth.with_db(
        url=mint_url, db=wallet_dir, username=payload["username"],
        password=payload["password"], client_id="cashu-client",
    )
    required = await wallet.init_auth_wallet(mint_auth_proofs=False, force_auth=True)
    if not required:
        raise RuntimeError("Nutshell wallet did not detect required authentication")
    claims = __import__("jwt").decode(
        wallet.oidc_client.access_token, options={"verify_signature": False}
    )
    if not claims.get("sub") or claims.get("iss") != discovery["issuer"] or claims.get("azp") != "cashu-client":
        metadata = {key: claims.get(key) for key in ("iss", "azp", "sub", "scope", "typ")}
        raise RuntimeError(f"Keycloak token claims do not satisfy Nutshell: {metadata}")
    def outputs(count):
        secrets = [hashlib.sha256(os.urandom(32)).hexdigest() for _ in range(count)]
        blinded, _ = wallet._construct_outputs(
            [1] * count, secrets, [PrivateKey(os.urandom(32)) for _ in range(count)]
        )
        return [entry.model_dump() for entry in blinded]
    async with httpx.AsyncClient(timeout=30) as client:
        headers = {"Clear-auth": wallet.oidc_client.access_token}
        excessive = await client.post(
            mint_url + "/v1/auth/blind/mint", json={"outputs": outputs(4)}, headers=headers
        )
        expect_code(excessive, 81003, "BAT maximum")
        accepted = await client.post(
            mint_url + "/v1/auth/blind/mint", json={"outputs": outputs(1)}, headers=headers
        )
        accepted.raise_for_status()
        signatures = accepted.json().get("signatures", [])
        if len(signatures) != 1 or not signatures[0].get("dleq"):
            raise RuntimeError(f"valid CAT did not mint one DLEQ-backed BAT: {accepted.text}")
        limited = await client.post(
            mint_url + "/v1/auth/blind/mint", json={"outputs": outputs(1)}, headers=headers
        )
        expect_code(limited, 81004, "CAT rate limit")
    print(json.dumps({
        "advertised_nut21": True, "advertised_nut22": True,
        "invalid_oidc_password_rejected": True, "missing_cat_rejected": True,
        "invalid_cat_code": 80002, "missing_bat_rejected": True,
        "invalid_bat_code": 81002, "valid_cat_bat_mint": True,
        "bat_max_code": 81003, "rate_limit_code": 81004,
    }))

asyncio.run(main())
