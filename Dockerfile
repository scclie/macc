FROM rust:1-slim AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY www ./www
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/* \
    && useradd --uid 20900 --create-home --shell /usr/sbin/nologin macc
WORKDIR /app
COPY --from=builder /app/target/release/macc /app/macc
USER macc
EXPOSE 8787
CMD ["/app/macc"]