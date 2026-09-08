"""Read Nutshell's sat proofs without importing or starting a wallet."""
import json
from pathlib import Path
import sqlite3

# Native wallets can have names different from their containing component.
paths = [p for p in Path('/wallet/.cashu').glob('*/*.sqlite3')
         if p.name == p.parent.name + '.sqlite3']
if not paths:
    raise ValueError('wallet not initialized')
balance = reserved = 0
for path in paths:
    with sqlite3.connect(path.as_uri() + '?mode=ro', uri=True, timeout=1) as db:
        db.execute('PRAGMA query_only=ON')
        db.execute('BEGIN')
        rows = db.execute("SELECT p.amount, COALESCE(p.reserved, 0) FROM proofs p WHERE EXISTS (SELECT 1 FROM keysets k WHERE k.id=p.id AND k.unit='sat')")
        for amount, held in rows:
            if type(amount) is not int or amount < 0:
                raise ValueError('invalid amount')
            if held:
                reserved += amount
            else:
                balance += amount
        if balance + reserved > 2**64 - 1:
            raise ValueError('balance overflow')
print(json.dumps({'balance_sat': balance, 'reserved_sat': reserved}))
