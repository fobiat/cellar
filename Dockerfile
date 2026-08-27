# Cellar, for adding to an existing s&box server image.
#
# This does not build a game server image. A game image should provide steamcmd,
# Wine, the Windows .NET runtime and the gamemode, then copy Cellar into it.
#
# Instead this is a builder whose output is copied into that image:
#
#     # in the game's server/Dockerfile
#     COPY --from=ghcr.io/fobiat/cellar:latest /cellar /usr/local/bin/cellar
#     ENTRYPOINT ["/usr/local/bin/cellar", "run"]
#
# replacing the image's entrypoint. Cellar then supervises `wine sbox-server.exe`
# on a pseudo-terminal, serves the operator UI and answers readiness probes.

FROM rust:1-bookworm AS build

WORKDIR /src

# Dependencies first, so a source change does not rebuild the whole tree.
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates

RUN cargo build --release -p cellar-cli \
    && strip target/release/cellar

# A scratch stage, so what gets copied into the server image is one static-ish
# binary and nothing else. Glibc is still linked, which is fine: the image this
# lands in is Debian or Alpine with Wine already in it.
FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=build /src/target/release/cellar /cellar
COPY cellar.toml.example /cellar.toml.example
COPY scripts/container-entrypoint.sh /usr/local/bin/cellar-entrypoint
RUN chmod 0755 /usr/local/bin/cellar-entrypoint

# The entrypoint prepares the default config, checks it, and starts Cellar.
# Override the command for `doctor`, `config`, or another CLI operation.
ENTRYPOINT ["/usr/local/bin/cellar-entrypoint"]
CMD ["run"]
