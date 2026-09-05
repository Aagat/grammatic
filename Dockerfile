FROM node:22-bookworm-slim AS frontend
WORKDIR /build/frontend
COPY frontend/package*.json ./
RUN npm ci
COPY frontend/ ./
RUN npm run build
FROM rust:1-bookworm AS backend
RUN apt-get update && apt-get install -y libdbus-1-dev pkg-config && rm -rf /var/lib/apt/lists/*
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY migrations ./migrations
RUN cargo build --release --locked
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y libdbus-1-3 libssl3 ca-certificates && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=backend /build/target/release/grammatic /usr/local/bin/grammatic
COPY --from=frontend /build/frontend/dist/client ./frontend/dist/client
COPY config.toml ./config.toml
USER 65532:65532
EXPOSE 8090
ENTRYPOINT ["grammatic"]
CMD ["serve", "--bind", "0.0.0.0:8090"]
