use std::process::Command;

#[test]
fn passive_cocod_reader_preserves_transactions_and_rejects_unknown_state() {
    let directory = tempfile::tempdir().unwrap();
    let contract = r#"
import hashlib, importlib.util, json, pathlib, sqlite3, sys
spec=importlib.util.spec_from_file_location('reader',sys.argv[1]); reader=importlib.util.module_from_spec(spec); spec.loader.exec_module(reader)
path=pathlib.Path(sys.argv[2])/'coco.db'; db=sqlite3.connect(path)
db.execute('PRAGMA journal_mode=WAL')
db.execute('CREATE TABLE coco_cashu_proofs(mintUrl TEXT, unit TEXT, state TEXT, amount TEXT, usedByOperationId TEXT, secret TEXT)')
db.execute('CREATE TABLE coco_cashu_mints(mintUrl TEXT)'); db.execute("INSERT INTO coco_cashu_mints VALUES('http://mint:3338')")
db.execute('CREATE TABLE coco_cashu_migrations(id TEXT)'); db.execute("INSERT INTO coco_cashu_migrations VALUES('038_keypair_derivation_allocations')")
db.executemany('INSERT INTO coco_cashu_proofs VALUES(?,?,?,?,?,?)',[
 ('http://mint:3338','sat','ready','64',None,'private-proof'),
 ('http://mint:3338','sat','ready','32','operation','private-proof'),
 ('http://mint:3338','sat','inflight','16',None,'private-proof'),
 ('http://mint:3338','sat','spent','128',None,'private-proof'),
 ('http://other:3338','sat','ready','1000',None,'private-proof'),
 ('http://mint:3338','msat','ready','1000',None,'private-proof')]); db.commit()
def read(): return reader.observe(path,'wallet','mint','http://mint:3338')
r=read(); assert [r[k] for k in ['balance_sat','reserved_sat','inflight_sat','total_ready_sat']]==[64,32,16,96]
assert 'pending_sat' not in r and 'private-proof' not in json.dumps(r)
db.execute("UPDATE coco_cashu_proofs SET amount='33' WHERE usedByOperationId='operation'")
assert read()['reserved_sat']==32
db.commit(); assert read()['reserved_sat']==33
for state,amount in [('unknown','1'),('ready','-1'),('ready','1.5'),('ready','18446744073709551616')]:
 db.execute('INSERT INTO coco_cashu_proofs VALUES(?,?,?,?,?,?)',('http://mint:3338','sat',state,amount,None,'private-proof')); db.commit()
 try: read()
 except ValueError: pass
 else: raise AssertionError('invalid proof accepted')
 db.execute('DELETE FROM coco_cashu_proofs WHERE rowid=(SELECT MAX(rowid) FROM coco_cashu_proofs)'); db.commit()
db.execute("INSERT INTO coco_cashu_migrations VALUES('039_unknown')"); db.commit()
try: read()
except ValueError: pass
else: raise AssertionError('future schema accepted')
db.execute("DELETE FROM coco_cashu_migrations WHERE id='039_unknown'"); db.commit(); db.close()
digest=hashlib.sha256(path.read_bytes()).digest(); assert read()['balance_sat']==64; assert hashlib.sha256(path.read_bytes()).digest()==digest
try: reader.observe(path,'wallet','mint','http://unknown:3338')
except ValueError: pass
else: raise AssertionError('unknown mint reported zero')
missing=path.parent/'missing.db'
try: reader.observe(missing,'wallet','mint','http://mint:3338')
except sqlite3.Error: pass
else: raise AssertionError('missing database reported zero')
assert not missing.exists()
"#;
    let result = Command::new("python3")
        .args([
            "-c",
            contract,
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/drivers/cocod_wallet_balance.py"
            ),
        ])
        .arg(directory.path())
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}
