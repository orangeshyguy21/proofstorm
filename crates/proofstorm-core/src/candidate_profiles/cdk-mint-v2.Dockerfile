# Build the mint like CDK's release binaries: a static musl binary with the
# workspace's release-static profile, on the plain Alpine runtime of its
# published image. One Cargo command compiles the crates the daemon and the
# management client share once; two jobs fit the 4 GiB build limit with LTO
# off. No Nix dev shell.
FROM rust:1-alpine@sha256:a10e64dd139b7387337c7fbe8aca31b959b57b2fd4c8ae20a02cf1d6ea424dce AS build
RUN apk add --no-cache musl-dev protobuf-dev curl make perl clang openssl-dev openssl-libs-static
# PostgreSQL TLS (cdk-postgres -> native-tls) needs OpenSSL; link it statically.
ENV OPENSSL_STATIC=1
# Keep release-static's runtime behaviour (opt-level 3, panic = "abort") but not
# its thin LTO and single codegen unit, which exceed the 4 GiB build limit.
ENV CARGO_PROFILE_RELEASE_STATIC_LTO=off CARGO_PROFILE_RELEASE_STATIC_CODEGEN_UNITS=16
WORKDIR /src
COPY . .
# Use the compiler pinned in rust-toolchain.toml, without the cross-compile
# targets and editor components that file also lists (minutes of download).
RUN if [ -f rust-toolchain.toml ]; then \
        export RUSTUP_TOOLCHAIN="$(sed -n 's/^channel = "\(.*\)"$/\1/p' rust-toolchain.toml)"; \
    fi && \
    cargo build --locked --profile release-static --jobs 2 \
    --features cdk-mintd/ldk-node,cdk-mintd/postgres,cdk-mintd/prometheus \
    --bin cdk-mintd --bin cdk-mint-cli

FROM alpine:3.21@sha256:ce64758a109eb420d874a118f87920e625e12d3634e03b4a5573fd9f6e5d3507
RUN apk add --no-cache ca-certificates
COPY --from=build /src/target/release-static/cdk-mintd /src/target/release-static/cdk-mint-cli /usr/local/bin/
# Readiness and Bitcoin dependency checks use the base image's busybox wget.
RUN cdk-mintd --version && cdk-mint-cli --version && command -v wget >/dev/null
CMD ["cdk-mintd"]
