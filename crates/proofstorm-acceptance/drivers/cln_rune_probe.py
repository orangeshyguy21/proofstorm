import hashlib, json, os, httpx
path = '/app/data/.proofstorm/cln.rune'
rune = open(path).read().strip()
headers = {'rune': rune, 'accept': 'application/json'}
allowed = httpx.post('http://mint-cln:3010/v1/listfunds', headers=headers).status_code
forbidden = httpx.post('http://mint-cln:3010/v1/withdraw', headers=headers, data={'destination': 'x', 'satoshi': 'all'}).status_code
print(json.dumps({'length': len(rune), 'mode': oct(os.stat(path).st_mode & 0o777), 'digest': hashlib.sha256(rune.encode()).hexdigest(), 'allowed': allowed, 'forbidden': forbidden}))
