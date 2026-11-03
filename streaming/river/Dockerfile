FROM rust:1.88.0-bookworm

WORKDIR /app
RUN apt-get update && apt-get install -y --no-install-recommends \
      build-essential cmake git libclang-dev libpng-dev libtiff-dev pkg-config \
    && rm -rf /var/lib/apt/lists/*
RUN git clone --depth 1 --branch v20.4.15 --recurse-submodules \
      https://github.com/GrokImageCompression/grok.git /tmp/grok \
    && cmake -S /tmp/grok -B /tmp/grok/build \
      -DCMAKE_BUILD_TYPE=Release \
      -DCMAKE_INSTALL_PREFIX=/usr/local \
      -DBUILD_TESTING=OFF \
      -DGRK_BUILD_CODEC=ON \
      -DGRK_BUILD_DOC=OFF \
      -DGRK_BUILD_LIBPNG=OFF \
      -DGRK_BUILD_LIBTIFF=OFF \
      -DGRK_BUILD_JPEG=OFF \
    && cmake --build /tmp/grok/build --parallel 4 \
    && cmake --install /tmp/grok/build \
    && ldconfig \
    && rm -rf /tmp/grok
COPY . .
RUN cargo build --release --package river-server --features grok,jxl,nvjpeg2000
COPY licenses/grok-AGPL-3.0.txt /usr/share/doc/river/licenses/GROK-AGPL-3.0.txt
COPY --from=nvjpeg-runtime /usr/local/lib/python3.12/dist-packages/nvidia/nvjpeg2k/lib/libnvjpeg2k.so.0 /usr/local/lib/libnvjpeg2k.so.0
COPY --from=nvjpeg-runtime /usr/local/cuda-13.3/targets/x86_64-linux/lib/libcudart.so.13.3.29 /usr/local/lib/libcudart.so.13
COPY --from=nvjpeg-runtime /usr/local/lib/python3.12/dist-packages/nvidia_nvjpeg2k_cu13-0.11.0.51.dist-info/licenses/LICENSE.txt /usr/share/doc/river/licenses/NVJPEG2000.txt

ENV RIVER_BIND=0.0.0.0:8080
CMD ["/app/target/release/river-server"]
