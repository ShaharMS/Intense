FROM rust:1.88.0-bookworm AS build
WORKDIR /app
COPY Cargo.lock ./Cargo.lock
COPY tools/range-sdk-lab/Cargo.toml ./Cargo.toml
COPY tools/range-sdk-lab/src ./src
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/target \
    cargo build --release --offline --jobs 2 && install -D target/release/range-sdk-replay /out/range-sdk-replay
FROM river-reader-lab:marked-packed-m212
COPY --from=build /out/range-sdk-replay /usr/local/bin/range-sdk-replay
COPY tools/curl-range-replay.cpp /tmp/curl-range-replay.cpp
RUN g++ -O3 -std=c++17 /tmp/curl-range-replay.cpp -lcurl -lcrypto -o /usr/local/bin/range-curl-replay
ENTRYPOINT ["sleep", "infinity"]
