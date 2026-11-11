# Image published with each release. It is built from the release
# archives, unpacked by the release workflow into dist/<arch>/, so the
# image holds exactly the binaries attached to the GitHub release.
# Same layout as the Dockerfile at the root, which builds from source.

FROM gcr.io/distroless/static-debian12:nonroot
ARG TARGETARCH
COPY dist/${TARGETARCH}/vericount/bin/vericount-server /usr/local/bin/vericount-server
COPY dist/${TARGETARCH}/vericount/web /app/web
ENV VERICOUNT_ADDR=0.0.0.0:8080 \
    VERICOUNT_WEB_DIR=/app/web
EXPOSE 8080
USER nonroot
HEALTHCHECK --interval=30s --timeout=6s --start-period=10s \
    CMD ["/usr/local/bin/vericount-server", "healthcheck"]
ENTRYPOINT ["/usr/local/bin/vericount-server"]
CMD ["serve"]
