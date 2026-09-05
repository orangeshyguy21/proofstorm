use std::{fs, process::Command};

#[test]
fn passive_reader_respects_live_transactions_scope_and_failure_semantics() {
    let directory = tempfile::tempdir().expect("test directory");
    let test = r#"
import hashlib, importlib.util, json, pathlib, sqlite3, sys
spec = importlib.util.spec_from_file_location('reader', sys.argv[1])
reader = importlib.util.module_from_spec(spec)
spec.loader.exec_module(reader)
root = pathlib.Path(sys.argv[2])
path = root / 'wallet with spaces.sqlite'
db = sqlite3.connect(path)
db.execute('PRAGMA journal_mode=WAL')
db.execute('CREATE TABLE proof (mint_url TEXT, unit TEXT, state TEXT, amount INTEGER)')
db.executemany('INSERT INTO proof VALUES (?,?,?,?)', [
 ('http://mint:3338','sat','UNSPENT',64),
 ('http://mint:3338','sat','RESERVED',32),
 ('http://mint:3338','sat','PENDING',16),
 ('http://mint:3338','sat','PENDING_SPENT',8),
 ('http://mint:3338','sat','SPENT',128),
 ('http://other:3338','sat','UNSPENT',1000),
 ('http://mint:3338','msat','UNSPENT',1000),
])
db.commit()
def read(): return reader.observe(path,'wallet','mint','http://mint:3338')
before = read()
assert [before[k] for k in ('balance_sat','reserved_sat','pending_sat','pending_spent_sat')] == [64,32,16,8]
assert 'secret' not in json.dumps(before)
db.execute("UPDATE proof SET amount=99 WHERE state='RESERVED'")
assert read()['reserved_sat'] == 32, 'reader saw uncommitted mutation'
db.commit()
assert read()['reserved_sat'] == 99
assert db.execute('SELECT COUNT(*) FROM proof').fetchone()[0] == 7
db.close()
digest = hashlib.sha256(path.read_bytes()).digest()
assert read()['balance_sat'] == 64
assert hashlib.sha256(path.read_bytes()).digest() == digest, 'reader modified database'
try: reader.observe(root/'missing.sqlite','wallet','mint','http://mint:3338')
except sqlite3.Error: pass
else: raise AssertionError('missing database reported zero')
assert not (root/'missing.sqlite').exists()
db=sqlite3.connect(path)
for state,amount in [('UNKNOWN',1),('UNSPENT',-1),('UNSPENT',1.5)]:
 db.execute('INSERT INTO proof VALUES (?,?,?,?)',('http://mint:3338','sat',state,amount)); db.commit()
 try: read()
 except ValueError: pass
 else: raise AssertionError('invalid proof accepted')
 db.execute('DELETE FROM proof WHERE rowid=(SELECT MAX(rowid) FROM proof)'); db.commit()
db.execute('DROP TABLE proof'); db.commit()
try: read()
except ValueError: pass
else: raise AssertionError('unknown schema reported zero')
print('passive CDK observation contract passed')
"#;
    let script = directory.path().join("contract.py");
    fs::write(&script, test).expect("write contract");
    let result = Command::new("python3")
        .arg(script)
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/drivers/cdk_wallet_balance.py"
        ))
        .arg(directory.path())
        .output()
        .expect("run Python observation contract");
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}
