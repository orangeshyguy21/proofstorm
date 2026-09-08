"""Read Nutshell's sat proofs without importing or starting a wallet."""
import json
from pathlib import Path
import sqlite3

# Native wallets can have names different from their containing component.
paths = [p for p in Path('/wallet/.cashu').glob('*/*.sqlite3')
         if p.name == p.parent.name + '.sqlite3']
if not paths:
    raise ValueError('wallet not initialized')
holdings = {}
for path in paths:
    with sqlite3.connect(path.as_uri() + '?mode=ro', uri=True, timeout=1) as db:
        db.execute('PRAGMA query_only=ON')
        db.execute('BEGIN')
        # A keyset must identify exactly one mint; do not double count joins.
        keysets = {}
        for keyset, mint_url in db.execute("SELECT id, mint_url FROM keysets WHERE unit='sat'"):
            if not isinstance(mint_url, str) or not mint_url:
                raise ValueError('mint unavailable')
            if keyset in keysets and keysets[keyset] != mint_url:
                raise ValueError('ambiguous keyset')
            keysets[keyset] = mint_url
        rows = db.execute("SELECT id, amount, COALESCE(reserved, 0) FROM proofs")
        for keyset, amount, held in rows:
            if keyset not in keysets:
                # Non-sat proofs are outside this view; an unknown keyset is not zero.
                if not db.execute("SELECT 1 FROM keysets WHERE id=?", (keyset,)).fetchone():
                    raise ValueError('unknown keyset')
                continue
            if type(amount) is not int or amount < 0:
                raise ValueError('invalid amount')
            row = holdings.setdefault(keysets[keyset], {'balance_sat': 0, 'reserved_sat': 0})
            row['reserved_sat' if held else 'balance_sat'] += amount
            if sum(row.values()) > 2**64 - 1:
                raise ValueError('balance overflow')
print(json.dumps({'mints': [dict(row, mint_url=url) for url, row in sorted(holdings.items())]}))
