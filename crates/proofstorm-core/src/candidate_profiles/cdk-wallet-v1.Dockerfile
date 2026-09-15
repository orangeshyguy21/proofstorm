# Compile the wallet from the verified candidate checkout, with its own lockfile.
FROM rust:1-alpine@sha256:a10e64dd139b7387337c7fbe8aca31b959b57b2fd4c8ae20a02cf1d6ea424dce AS build
RUN apk add --no-cache musl-dev protobuf-dev curl make perl clang
WORKDIR /src
COPY . .
RUN cargo build --locked --release --bin cdk-cli
FROM debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates libssl3 && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/cdk-cli /usr/local/bin/cdk-cli
RUN cdk-cli --version && cdk-cli --help
ENV HOME=/wallet
USER 1000:1000
ENTRYPOINT ["/bin/sh", "-c", "umask 077; trap 'exit 0' TERM INT; while :; do sleep 3600 & wait $!; done"]
