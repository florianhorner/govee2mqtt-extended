ARG BUILD_FROM
# The add-on copies its binary out of the standalone govee2mqtt image. CI pins
# this to the release being built. The merge job only refreshes :latest on
# main, so a tag build reading :latest can package a different commit than the
# tag. The default keeps Supervisor local builds, which pass only BUILD_FROM,
# working unchanged.
ARG GOVEE_IMAGE=ghcr.io/florianhorner/govee2mqtt:latest
FROM ${GOVEE_IMAGE} AS govee2mqtt
FROM $BUILD_FROM
COPY run.sh /run.sh
COPY --from=govee2mqtt /app/govee /app/
COPY --from=govee2mqtt /app/assets /app/assets/
COPY --from=govee2mqtt /app/AmazonRootCA1.pem /app/
CMD [ "/run.sh" ]
