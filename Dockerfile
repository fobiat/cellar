# Cellar, for adding to an existing s&box server image.
#
# This does not build a game server image. A game image should provide the
# s&box server, its runtime dependencies and the gamemode, then use this image
# as its Cellar layer. Choose `launcher = "native"` for a native server or
# explicitly install Wine and choose `launcher = "wine"` for a Windows binary.
#
# Instead this is a builder whose output is copied into that image:
#
#     # in the game's server/Dockerfile
#     COPY --from=ghcr.io/fobiat/cellar:latest /cellar /usr/local/bin/cellar
#     ENTRYPOINT ["/usr/local/bin/cellar", "run"]
#
# replacing the image's entrypoint. Cellar supervises the configured server on
# a pseudo-terminal, serves the operator UI and answers readiness probes.

FROM rust:1.88-bookworm AS build

WORKDIR /src

# Dependencies first, so a source change does not rebuild the whole tree.
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates

RUN cargo build --release -p cellar-cli \
    && strip target/release/cellar

# The final stage is a Cellar layer, not a complete game image. Glibc is linked
# dynamically, so the image that extends this stage must provide a compatible
# userspace and the configured game runtime.
FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=build /src/target/release/cellar /cellar
COPY cellar.toml.example /cellar.toml.example
COPY scripts/container-entrypoint.sh /usr/local/bin/cellar-entrypoint
RUN chmod 0755 /usr/local/bin/cellar-entrypoint

VOLUME ["/home/container/sbox/data", "/home/container/sbox/logs", "/var/lib/cellar/persistence", "/var/lib/cellar/backups"]

# The entrypoint prepares the default config, checks it, and starts Cellar.
# Override the command for `doctor`, `config`, or another CLI operation.
ENTRYPOINT ["/usr/local/bin/cellar-entrypoint"]
CMD ["run"]
