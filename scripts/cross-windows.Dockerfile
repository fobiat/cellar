# Toolchain for building Cellar's Windows binaries from Linux.
#
# The GNU target rather than MSVC: it needs only mingw, which apt has, where the
# MSVC target needs the Windows SDK headers pulled from Microsoft. The resulting
# .exe has no runtime dependency on either.
FROM rust:1-bookworm

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        gcc-mingw-w64-x86-64 \
        g++-mingw-w64-x86-64 \
        cmake \
        nasm \
        ninja-build \
    && rm -rf /var/lib/apt/lists/*

RUN rustup target add x86_64-pc-windows-gnu

# Point the linker at mingw. Cargo reads this rather than needing a
# .cargo/config.toml in the repo, which would then apply to host builds too.
ENV CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc \
    CC_x86_64_pc_windows_gnu=x86_64-w64-mingw32-gcc \
    CXX_x86_64_pc_windows_gnu=x86_64-w64-mingw32-g++ \
    AR_x86_64_pc_windows_gnu=x86_64-w64-mingw32-ar
