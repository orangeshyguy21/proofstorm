import json
from urllib.parse import urlparse
from cashu.core.settings import settings
url = urlparse(settings.mint_redis_cache_url)
print(json.dumps({
    'enabled': settings.mint_redis_cache_enabled,
    'host': url.hostname,
    'password_length': len(url.password or ''),
    'ttl': settings.mint_redis_cache_ttl,
    'cluster': settings.mint_redis_cache_cluster,
}))
