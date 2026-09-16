FROM oven/bun:1.3.10@sha256:b86c67b531d87b4db11470d9b2bd0c519b1976eee6fcd71634e73abfa6230d2e AS build
WORKDIR /opt/coco
COPY . .
RUN bun install --frozen-lockfile --ignore-scripts
RUN bun run --filter=@cashu/coco-core build && bun run --filter=@cashu/coco-sql-storage build && bun run --filter=@cashu/coco-sqlite-bun build
FROM debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates libstdc++6 && rm -rf /var/lib/apt/lists/*
COPY --from=build /usr/local/bin/bun /usr/local/bin/bun
COPY --from=build /opt/coco /opt/coco
RUN printf '#!/bin/sh\nexec bun /opt/coco/packages/cocod/src/index.ts "$@"\n' > /usr/local/bin/cocod && chmod 755 /usr/local/bin/cocod && cocod --version && cocod --help
ENV HOME=/wallet
WORKDIR /wallet
USER 1000:1000
CMD ["cocod", "daemon"]
