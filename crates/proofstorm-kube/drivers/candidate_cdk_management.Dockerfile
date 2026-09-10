# Build the native client from the same frozen candidate source as the daemon.
FROM rust:1-alpine@sha256:a10e64dd139b7387337c7fbe8aca31b959b57b2fd4c8ae20a02cf1d6ea424dce AS proofstorm-management-client
RUN apk add --no-cache musl-dev protobuf-dev curl make perl clang
WORKDIR /src
COPY . .
RUN cargo build --locked --release --bin cdk-mint-cli

