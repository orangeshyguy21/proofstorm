import json
from cashu.core.settings import settings
print(json.dumps({
    'version': settings.version,
    'name': settings.mint_info_name,
    'description': settings.mint_info_description,
    'input_fee_ppk': settings.mint_input_fee_ppk,
    'mint_quote_ttl': settings.mint_quote_ttl,
    'melt_quote_ttl': settings.melt_quote_ttl,
    'max_mint_sat': settings.mint_max_mint_bolt11_sat,
    'max_melt_sat': settings.mint_max_melt_bolt11_sat,
    'max_balance_sat': settings.mint_max_balance,
    'global_rate_limit': settings.mint_global_rate_limit_per_minute,
    'transaction_rate_limit': settings.mint_transaction_rate_limit_per_minute,
    'lightning_fee_percent': settings.lightning_fee_percent,
    'lightning_reserve_fee_min': settings.lightning_reserve_fee_min,
    'backend': settings.mint_backend_bolt11_sat,
    'lnd_endpoint': settings.mint_lnd_rest_endpoint,
    'database': settings.mint_database,
    'private_key_length': len(settings.mint_private_key or ''),
}))
