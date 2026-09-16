# Proofstorm's readiness and Bitcoin dependency checks run wget inside the mint.
# The upstream Debian runtime installs patchelf but does not provide wget.
RUN apt-get update && \
    apt-get install -y --no-install-recommends wget ca-certificates && \
    rm -rf /var/lib/apt/lists/*
COPY --from=proofstorm-management-client /src/target/release/cdk-mint-cli /usr/local/bin/cdk-mint-cli
RUN cdk-mintd --version && cdk-mint-cli --version && wget --version >/dev/null
