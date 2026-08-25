# Cellar, for adding to an existing s&box server image.
#
# This does not build a server image. AppleJackRP's own `server/Dockerfile`
# already bakes in steamcmd, Wine, the Windows .NET runtime and the gamemode,
# and duplicating that here would be a second copy to keep working.
#
# Instead this is a builder whose output is copied into that image:
#
#     # in AppleJackRP-sandbox/server/Dockerfile
#     COPY --from=ghcr.io/fobiat/cellar:latest /cellar /usr/local/bin/cellar
#     ENTRYPOINT ["/usr/local/bin/cellar", "run"]
#
# replacing entrypoint.sh. Cellar then supervises `wine sbox-server.exe` on a
# pseudo-terminal, serves the bridge and answers the readiness probe the
# deployment asked for in a comment.

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

# Answering `--version` is the cheapest proof the binary in this image runs at
# all, and it is what a `docker run --rm ghcr.io/fobiat/cellar` should print.
ENTRYPOINT ["/cellar"]
CMD ["--version"]
