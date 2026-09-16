# Disposable contracts against exact, locally built component and helper images.
# Build each target with explicit image arguments; RUN has no external network.
ARG DRIVER_IMAGE=proofstorm-driver:explicit-image-required
ARG COCO_IMAGE=proofstorm-coco:explicit-image-required
ARG NUTSHELL_IMAGE=proofstorm-nutshell:explicit-image-required
ARG CDK_IMAGE=proofstorm-cdk:explicit-image-required
ARG BITCOIN_IMAGE=proofstorm-bitcoin:explicit-image-required
FROM ${DRIVER_IMAGE} AS driver

FROM ${BITCOIN_IMAGE} AS bitcoin
COPY bitcoin.sh /contract.sh
RUN --network=none timeout 90 sh /contract.sh

FROM ${CDK_IMAGE} AS cdk
COPY --from=driver /proofstorm-driver /opt/proofstorm/driver
COPY cdk.sh /contract.sh
RUN --network=none timeout 30 sh /contract.sh

FROM ${COCO_IMAGE} AS coco
USER root
COPY --from=driver /proofstorm-driver /opt/proofstorm/driver
COPY coco.sh /contract.sh
RUN mkdir -p /wallet && chown 1000:1000 /wallet
USER 1000:1000
RUN --network=none timeout 90 bash /contract.sh

FROM ${NUTSHELL_IMAGE} AS nutshell
ARG NUTSHELL_VERSION=0.20.3
ENV PROOFSTORM_NUTSHELL_VERSION=${NUTSHELL_VERSION}
USER root
COPY --from=driver /proofstorm-driver /opt/proofstorm/driver
COPY nutshell.sh /contract.sh
COPY nutshell-wallet.sh /wallet-contract.sh
RUN mkdir -p /wallet /app/data /management-client/tls && chown -R 1000:1000 /wallet /app/data /management-client
USER 1000:1000
RUN --network=none timeout 90 bash /contract.sh
RUN --network=none timeout 120 bash /wallet-contract.sh
