FROM nvidia/cuda:13.3.0-base-ubuntu24.04

ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
      python3 python3-pip \
    && rm -rf /var/lib/apt/lists/*
RUN python3 -m pip install --break-system-packages --no-cache-dir \
      'nvidia-nvjpeg2k-cu13==0.11.0.51' numpy pillow
RUN python3 -c "import ctypes; ctypes.CDLL('liblz4.so.1')"

WORKDIR /app
COPY server.py /app/server.py
COPY native.py /app/native.py
COPY resolution_view.py /app/resolution_view.py
COPY rpc_codec.py /app/rpc_codec.py
ENV PORT=8081
EXPOSE 8081
CMD ["python3", "/app/server.py"]
