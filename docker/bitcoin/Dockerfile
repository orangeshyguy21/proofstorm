# Bitcoin Core release binaries; packaging only, with no daemon source changes.
# Both stages use the same reviewed Debian image. Only the fetch stage installs tools.
FROM debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171 AS fetch
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl gnupg \
    && rm -rf /var/lib/apt/lists/*
ARG TARGETARCH
WORKDIR /release
RUN set -eu; \
    base=https://bitcoincore.org/bin/bitcoin-core-31.1; \
    curl --fail --location --retry 3 "$base/SHA256SUMS" -o SHA256SUMS; \
    echo 'e63028d0bb33d22c847479d73866be8c1142246911765c005efbf611b645e706  SHA256SUMS' | sha256sum -c -; \
    curl --fail --location --retry 3 "$base/SHA256SUMS.asc" -o SHA256SUMS.asc; \
    curl --fail --location --retry 3 https://raw.githubusercontent.com/bitcoin-core/guix.sigs/3b667ee3ebb3dcd9e1990cf03e38a0935eec1683/builder-keys/fanquake.gpg -o signer.gpg; \
    mkdir -m 700 gnupg; \
    gpg --batch --homedir gnupg --import signer.gpg; \
    gpg --batch --homedir gnupg --status-fd 1 --verify SHA256SUMS.asc SHA256SUMS > signature-status 2> signature-log || true; \
    awk '$2 == "VALIDSIG" && $3 == "CFB16E21C950F67FA95E558F2EEB9F5CC09526C1" && $NF == "E777299FC265DD04793070EB944D35F9AC3DB76A" { valid = 1 } END { exit !valid }' signature-status \
      || { cat signature-log; exit 1; }; \
    case "$TARGETARCH" in amd64) arch=x86_64 ;; arm64) arch=aarch64 ;; *) echo "Unsupported architecture: $TARGETARCH" >&2; exit 1 ;; esac; \
    archive="bitcoin-31.1-$arch-linux-gnu.tar.gz"; \
    curl --fail --location --retry 3 "$base/$archive" -o "$archive"; \
    awk -v name="$archive" '$2 == name { print; found = 1 } END { if (!found) exit 1 }' SHA256SUMS > archive-checksum; \
    sha256sum -c archive-checksum; \
    tar -xzf "$archive"; \
    install -d /verified; \
    cp bitcoin-31.1/bin/* /verified/

FROM debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171
COPY --from=fetch /verified/ /usr/local/bin/
RUN install -d -o 1000 -g 1000 /home/bitcoin/.bitcoin
LABEL org.opencontainers.image.source="https://github.com/bitcoin/bitcoin" \
      org.opencontainers.image.revision="9be056a8a72b624dae9623b2f7bded92c2a21c91" \
      org.opencontainers.image.version="31.1"
ENV HOME=/home/bitcoin
USER 1000:1000
RUN bitcoind --version && bitcoin-cli --version
ENTRYPOINT ["bitcoind"]
