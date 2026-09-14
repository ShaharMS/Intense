FROM rust:1.88.0-bookworm

WORKDIR /app
COPY . .
RUN cargo build --release --package river-server

ENV RIVER_BIND=0.0.0.0:8080
CMD ["/app/target/release/river-server"]
