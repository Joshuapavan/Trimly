# syntax=docker/dockerfile:1
#
# The Trimly web server and web app in one image.
#   docker build -t trimly .
#   docker run -p 8080:8080 -v trimly-data:/data trimly
# then open http://localhost:8080

# ---- 1. the web app
FROM node:22-bookworm-slim AS web
WORKDIR /app
COPY package.json package-lock.json ./
RUN npm ci --ignore-scripts
COPY angular.json tsconfig.json tsconfig.app.json ./
COPY public ./public
COPY src ./src
RUN npx ng build

# ---- 2. the server
FROM rust:1-slim-bookworm AS server
WORKDIR /app
RUN apt-get update && apt-get install -y --no-install-recommends pkg-config ca-certificates && rm -rf /var/lib/apt/lists/*
# Cargo reads every workspace member, so the desktop crate's manifest is needed
# even though only the server is built here.
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY server ./server
COPY src-tauri ./src-tauri
RUN cargo build --release -p trimly-server

# ---- 3. the image that runs
FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ffmpeg python3 python3-venv ca-certificates curl \
 && python3 -m venv /opt/ytdlp \
 && /opt/ytdlp/bin/pip install --no-cache-dir "yt-dlp[default]" \
 && ln -s /opt/ytdlp/bin/yt-dlp /usr/local/bin/yt-dlp \
 && rm -rf /var/lib/apt/lists/*
# yt-dlp needs a JavaScript runtime to read YouTube.
COPY --from=denoland/deno:bin /deno /usr/local/bin/deno

COPY --from=server /app/target/release/trimly-server /usr/local/bin/trimly-server
COPY --from=web /app/dist/trimly/browser /app/web

RUN useradd --create-home --uid 10001 trimly && mkdir -p /data && chown trimly /data
USER trimly
ENV PORT=8080 \
    TRIMLY_STATIC_DIR=/app/web \
    TRIMLY_DATA_DIR=/data \
    HOME=/home/trimly
VOLUME /data
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s \
  CMD curl -fsS "http://localhost:${PORT}/api/health" || exit 1
CMD ["trimly-server"]
