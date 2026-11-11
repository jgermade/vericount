# vericounts image built from source: `docker build -t vericounts .`
#
# The server is a static (musl) binary, so the final image only holds it
# and the web client, on a distroless base without a shell.

FROM node:22-bookworm-slim AS web
WORKDIR /src/web
COPY web/package.json web/package-lock.json ./
RUN npm ci
COPY web ./
RUN npm run build

FROM rust:1.97.0-bookworm AS server
RUN apt-get update && apt-get install -y --no-install-recommends musl-tools \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
# The image's toolchain is the one rust-toolchain.toml pins; that file is
# left out so rustup does not fetch the components only development needs.
RUN rustup target add "$(uname -m)-unknown-linux-musl"
COPY Cargo.toml Cargo.lock ./
COPY crates crates
RUN cargo build --release --locked -p vericounts-server --target "$(uname -m)-unknown-linux-musl" \
    && cp "target/$(uname -m)-unknown-linux-musl/release/vericounts-server" /vericounts-server \
    && strip /vericounts-server

FROM gcr.io/distroless/static-debian12:nonroot
COPY --from=server /vericounts-server /usr/local/bin/vericounts-server
COPY --from=web /src/web/dist /app/web
ENV VERICOUNTS_ADDR=0.0.0.0:8080 \
    VERICOUNTS_WEB_DIR=/app/web
EXPOSE 8080
USER nonroot
HEALTHCHECK --interval=30s --timeout=6s --start-period=10s \
    CMD ["/usr/local/bin/vericounts-server", "healthcheck"]
ENTRYPOINT ["/usr/local/bin/vericounts-server"]
CMD ["serve"]
