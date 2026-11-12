# CINVAR cloud worker image -- TEMPLATE, NOT BUILT OR TESTED IN THIS REPO'S CI.
# Build and push it to a registry you control, then pass the image name to the cloud render tool.
#   docker build -f deploy/runpod/Dockerfile -t YOUR_REGISTRY/cinvar-worker:0.1 .
# The worker reads CINVAR_WORKER_TOKEN and CINVAR_WORKER_MAX_RUNTIME_SECS from the pod
# environment (set by CINVAR when it creates the pod) and exposes port 8000/http.
FROM rust:1.85-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked

FROM nvidia/cuda:12.4.1-runtime-ubuntu22.04
# NOTE: Ubuntu's ffmpeg is built with libx264/libx265 (GPL). NVENC needs an NVENC-enabled
# ffmpeg build; verify with `ffmpeg -encoders | grep nvenc` inside the image before relying on it.
RUN apt-get update && apt-get install -y --no-install-recommends ffmpeg fonts-dejavu-core ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/cinvar /usr/local/bin/cinvar
EXPOSE 8000
CMD ["cinvar", "worker", "--bind", "0.0.0.0:8000", "--root", "/workspace/cinvar-worker"]
