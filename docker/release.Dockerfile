# Image published with each release. It is built from the release
# archives, unpacked by the release workflow into dist/<arch>/, so the
# image holds exactly the binaries attached to the GitHub release.
# Same layout as the Dockerfile at the root, which builds from source.

FROM gcr.io/distroless/static-debian12:nonroot
ARG TARGETARCH
COPY dist/${TARGETARCH}/vericounts/bin/vericounts-server /usr/local/bin/vericounts-server
COPY dist/${TARGETARCH}/vericounts/web /app/web
ENV VERICOUNTS_ADDR=0.0.0.0:8080 \
    VERICOUNTS_WEB_DIR=/app/web
EXPOSE 8080
USER nonroot
HEALTHCHECK --interval=30s --timeout=6s --start-period=10s \
    CMD ["/usr/local/bin/vericounts-server", "healthcheck"]
ENTRYPOINT ["/usr/local/bin/vericounts-server"]
CMD ["serve"]
