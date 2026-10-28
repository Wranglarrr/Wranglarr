# wranglarr: one static binary (templates, static files and community configs are
# embedded) on Alpine with the Postgres 17 client tools.
#   docker build -t wranglarr:local wranglarr
# pinned; Dependabot (.github/dependabot.yml) proposes the updates
FROM rust:1.99.1-alpine3.24 AS build
RUN apk add --no-cache musl-dev openssl-dev openssl-libs-static pkgconf
ENV OPENSSL_STATIC=1
WORKDIR /src
COPY . .
# The target cache is shared by every build on the host, from any checkout. Cargo
# decides what to rebuild by modification time, so give the copied sources a fresh
# one: the dependencies stay cached, and the workspace's own crates always rebuild
# instead of reusing what another checkout compiled.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    find . -path ./target -prune -o -type f -exec touch {} + && \
    cargo build --release --locked && cp target/release/wranglarr /wranglarr

FROM alpine:3.24
# pg_dump / pg_restore / psql matching the Postgres 17 server in compose.yaml
RUN apk add --no-cache postgresql17-client tzdata ca-certificates
COPY --from=build /wranglarr /usr/local/bin/wranglarr
# one binary, several names (it picks the command from the name it's run as); proxy-setup
# also bare, for `docker compose exec -T wranglarr proxy-setup > …`
RUN for n in setup prepare auth apps-auth proxy-setup; do ln -s wranglarr "/usr/local/bin/wranglarr-$n"; done && \
    ln -s wranglarr /usr/local/bin/proxy-setup
# the image's own name: the setup wizard writes it to .env as WRANGLARR_IMAGE, so the stack
# runs the image the wizard came from (the release workflow passes the hosted name)
ARG WRANGLARR_IMAGE=wranglarr:local
ENV WRANGLARR_CONFIG=/config/wranglarr.toml WRANGLARR_IN_DOCKER=1 WRANGLARR_IMAGE=${WRANGLARR_IMAGE}
# where it comes from, for update checkers and release-note lookups (the release workflow
# sets the same, plus the version and commit: keep the two in step)
LABEL org.opencontainers.image.title="wranglarr" \
      org.opencontainers.image.description="Backup, version, restore and update a *arr stack on Postgres" \
      org.opencontainers.image.source="https://github.com/Wranglarrr/Wranglarr" \
      org.opencontainers.image.url="https://wranglarrr.github.io/Wranglarr/" \
      org.opencontainers.image.documentation="https://wranglarrr.github.io/Wranglarr/" \
      org.opencontainers.image.licenses="GPL-3.0-or-later"
EXPOSE 8787 8788
STOPSIGNAL SIGTERM
CMD ["wranglarr"]
