import asyncio, json, os, sys
import httpx
from cashu.core.base import AuthProof
from cashu.wallet.auth.auth import WalletAuth

payload = json.load(sys.stdin)
mint_url = "http://127.0.0.1:3338"

async def main():
    wallet_dir = "/tmp/proofstorm-nutshell-auth-post"
    os.makedirs(wallet_dir, mode=0o700, exist_ok=True)
    wallet = await WalletAuth.with_db(
        url=mint_url, db=wallet_dir, username=payload["username"],
        password=payload["password"], client_id="cashu-client",
    )
    required = await wallet.init_auth_wallet(mint_auth_proofs=True, force_auth=True)
    if not required or len(wallet.proofs) != 3:
        raise RuntimeError(f"Nutshell wallet minted {len(wallet.proofs)} BATs after restart")
    if any(proof.dleq is None for proof in wallet.proofs):
        raise RuntimeError("mint returned a BAT without its NUT-12 DLEQ proof")
    token = AuthProof.from_proof(wallet.proofs[0]).to_base64()
    async with httpx.AsyncClient(timeout=30) as client:
        quote = await client.post(
            mint_url + "/v1/mint/quote/bolt11",
            json={"amount": 1, "unit": "sat"}, headers={"Blind-auth": token},
        )
        quote.raise_for_status()
        quote_id = quote.json().get("quote")
        if not quote_id:
            raise RuntimeError(f"protected quote did not return an id: {quote.text}")
    with open("/app/data/proofstorm-oidc-used-bat", "w", encoding="utf-8") as handle:
        os.chmod(handle.fileno(), 0o600)
        handle.write(token)
    print(json.dumps({"nutshell_client_bats": 3, "dleq": True, "protected_quote": True}))

asyncio.run(main())
