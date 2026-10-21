FROM rust:1-slim AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY www ./www
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && dpkg --compare-versions "$(dpkg-query -W -f='${Version}' ca-certificates)" ge '20250419~deb12u2' \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --uid 20900 --create-home --shell /usr/sbin/nologin macc
WORKDIR /app
COPY --from=builder /app/target/release/macc /app/macc
USER macc
EXPOSE 8787
CMD ["/app/macc"]
