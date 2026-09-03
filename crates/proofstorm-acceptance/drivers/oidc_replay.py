import asyncio, json, os, sys
import httpx
from cashu.core.base import AuthProof
from cashu.wallet.auth.auth import WalletAuth

payload = json.load(sys.stdin)
mint_url = "http://127.0.0.1:3338"

async def main():
    with open("/app/data/proofstorm-oidc-used-bat", encoding="utf-8") as handle:
        spent_token = handle.read()
    async with httpx.AsyncClient(timeout=30) as client:
        replay = await client.post(
            mint_url + "/v1/mint/quote/bolt11",
            json={"amount": 1, "unit": "sat"}, headers={"Blind-auth": spent_token},
        )
        body = replay.json()
        if replay.status_code < 400 or body.get("code") != 81002:
            raise RuntimeError(f"spent BAT replay survived mint restart: {replay.status_code} {body}")
    wallet_dir = "/tmp/proofstorm-nutshell-auth-recovered"
    os.makedirs(wallet_dir, mode=0o700, exist_ok=True)
    wallet = await WalletAuth.with_db(
        url=mint_url, db=wallet_dir, username=payload["username"],
        password=payload["password"], client_id="cashu-client",
    )
    await wallet.init_auth_wallet(mint_auth_proofs=True, force_auth=True)
    if len(wallet.proofs) != 3:
        raise RuntimeError("mint could not issue fresh BATs after restart")
    fresh = AuthProof.from_proof(wallet.proofs[0]).to_base64()
    async with httpx.AsyncClient(timeout=30) as client:
        recovered = await client.post(
            mint_url + "/v1/mint/quote/bolt11",
            json={"amount": 1, "unit": "sat"}, headers={"Blind-auth": fresh},
        )
        recovered.raise_for_status()
    os.remove("/app/data/proofstorm-oidc-used-bat")
    print(json.dumps({"spent_bat_replay_code": 81002, "fresh_cat": True, "fresh_bat": True}))

asyncio.run(main())
