FROM rust:1.85-bookworm AS git-remote-builder

RUN cargo install git-remote-htree --root /opt/git-remote-htree

FROM node:22-bookworm

ENV DEBIAN_FRONTEND=noninteractive
ENV HOME=/home/node
ENV PLAYWRIGHT_BROWSERS_PATH=/ms-playwright

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    curl \
    git \
    python3 \
    build-essential \
    && corepack enable \
    && corepack prepare pnpm@9.15.1 --activate \
    && mkdir -p /ms-playwright \
    && npx -y playwright@1.49.0 install --with-deps chromium \
    && rm -rf /var/lib/apt/lists/*

COPY --from=git-remote-builder /opt/git-remote-htree/bin/git-remote-htree /usr/local/bin/git-remote-htree

RUN chown -R node:node /ms-playwright
