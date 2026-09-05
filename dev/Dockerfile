FROM node:22-bookworm-slim AS frontend-dev
WORKDIR /workspace/frontend
COPY frontend/package.json frontend/package-lock.json ./
RUN npm ci
COPY frontend/ ./
CMD ["npm", "run", "dev", "--", "--host", "0.0.0.0", "--port", "5173", "--strictPort"]

FROM frontend-dev AS frontend-build
RUN npm run build && npm test

FROM rust:1-bookworm AS backend-dev
RUN apt-get update && apt-get install -y --no-install-recommends libdbus-1-dev pkg-config curl python3 \
    && rm -rf /var/lib/apt/lists/* \
    && rustup component add clippy
WORKDIR /workspace
COPY Cargo.toml Cargo.lock ./
COPY src/ ./src/
COPY migrations/ ./migrations/
COPY tests/ ./tests/
COPY examples/ ./examples/
COPY dev/config.toml dev/test-config.toml dev/test-backend.sh ./dev/
RUN cargo build --locked
CMD ["cargo", "run", "--locked", "--", "--config", "dev/config.toml", "serve", "--bind", "0.0.0.0:8090"]

FROM backend-dev AS tests
COPY --from=frontend-build /workspace/frontend/dist/client ./frontend/dist/client
CMD ["bash", "dev/test-backend.sh"]
