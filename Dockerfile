# syntax=docker/dockerfile:1
FROM rust:1-alpine AS build
RUN apk add --no-cache musl-dev
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY web ./web
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked && cp target/release/fsend /fsend
RUN mkdir -p /data

FROM gcr.io/distroless/static-debian13:nonroot
COPY --from=build /fsend /usr/local/bin/fsend
COPY --from=build --chown=65532:65532 /data /data
ENV FSEND_DATA_DIR=/data \
    FSEND_LISTEN=0.0.0.0:8080
VOLUME /data
EXPOSE 8080
USER 65532:65532
HEALTHCHECK --interval=30s --timeout=5s --retries=3 CMD ["/usr/local/bin/fsend", "--healthcheck"]
ENTRYPOINT ["/usr/local/bin/fsend"]
