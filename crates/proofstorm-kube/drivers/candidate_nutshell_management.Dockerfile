
# Expose the native entrypoint; do not translate commands or connection flags.
RUN printf '%s\n' '#!/bin/sh' 'cd /app && exec python3 -c '\''from cashu.mint.management_rpc.cli.cli import cli; cli()'\'' "$@"' > /usr/local/bin/mint-cli && chmod 755 /usr/local/bin/mint-cli && mint-cli --help
