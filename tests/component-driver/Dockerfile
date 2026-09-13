# Disposable contracts against exact, locally built component and helper images.
# Build each target with explicit image arguments; RUN has no external network.
ARG DRIVER_IMAGE=proofstorm-driver:explicit-image-required
ARG COCO_IMAGE=proofstorm-coco:explicit-image-required
ARG NUTSHELL_IMAGE=proofstorm-nutshell:explicit-image-required
ARG CDK_IMAGE=proofstorm-cdk:explicit-image-required
FROM ${DRIVER_IMAGE} AS driver

FROM ${CDK_IMAGE} AS cdk
COPY --from=driver /proofstorm-driver /opt/proofstorm/driver
COPY cdk.sh /contract.sh
RUN --network=none timeout 10 sh /contract.sh

FROM ${COCO_IMAGE} AS coco
USER root
COPY --from=driver /proofstorm-driver /opt/proofstorm/driver
COPY coco.sh /contract.sh
RUN mkdir -p /wallet && chown 1000:1000 /wallet
USER 1000:1000
RUN --network=none timeout 90 bash /contract.sh

FROM ${NUTSHELL_IMAGE} AS nutshell
USER root
COPY --from=driver /proofstorm-driver /opt/proofstorm/driver
COPY nutshell.sh /contract.sh
RUN mkdir -p /wallet /app/data /management-client/tls && chown -R 1000:1000 /wallet /app/data /management-client
USER 1000:1000
RUN --network=none timeout 90 bash /contract.sh
