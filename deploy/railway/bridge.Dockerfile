FROM rust:1.92.0-bookworm AS rust-build
WORKDIR /build/service
COPY service/ ./
RUN cargo build --release --locked -p app --bin bridge-auto -j 2

FROM node:24-bookworm-slim AS sidecar-build
WORKDIR /app/sidecar
COPY sidecar/package*.json ./
RUN npm ci --omit=dev --ignore-scripts --no-audit --no-fund

FROM node:24-bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates python3 \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=rust-build /build/service/target/release/bridge-auto /usr/local/bin/bridge-auto
COPY --from=sidecar-build /app/sidecar/ /app/sidecar/
COPY sidecar/src/ /app/sidecar/src/
COPY deploy/railway/start.py /app/start.py
ENV NODE_ENV=production BRIDGE_HTTP_HOST=0.0.0.0
USER node
CMD ["python3", "/app/start.py"]
