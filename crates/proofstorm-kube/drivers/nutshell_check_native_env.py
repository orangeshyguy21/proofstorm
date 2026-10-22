import sys

from cashu.core.settings import Settings

# Nutshell ignores environment variables it has no setting for, so a misspelled
# native_env name would be dropped silently. Fail before the mint starts.
known = {name.upper() for name in Settings.model_fields}
unknown = [name for name in sys.argv[1:] if name.upper() not in known]
if unknown:
    sys.exit("native_env names this Nutshell build does not read: " + ", ".join(unknown))
