import json
from urllib.parse import urlparse
from cashu.core.settings import settings
url = urlparse(settings.mint_database)
print(json.dumps({'version': settings.version, 'name': settings.mint_info_name, 'database_host': url.hostname, 'database_name': url.path.lstrip('/'), 'private_key_length': len(settings.mint_private_key or '')}))
