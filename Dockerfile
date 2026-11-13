# Headless Linux build environment for the starpod workspace.
# Rust is not installed on the Windows host; all builds run here.
FROM rust:1.92-bookworm

ENV DEBIAN_FRONTEND=noninteractive \
    CARGO_TERM_COLOR=always \
    CARGO_TARGET_DIR=/target

# System libraries needed by Slint (fontconfig/freetype/xkbcommon/wayland/x11
# winit + femtovg GL/EGL), dbus for secret-service, and general tooling.
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        pkg-config \
        libfontconfig1-dev \
        libfreetype-dev \
        libxkbcommon-dev \
        libxkbcommon-x11-dev \
        libwayland-dev \
        libx11-dev \
        libxcursor-dev \
        libxrandr-dev \
        libxi-dev \
        libgl1-mesa-dev \
        libegl1-mesa-dev \
        libdbus-1-dev \
        xvfb \
        xauth \
        x11-utils \
        imagemagick \
        xdotool \
        fonts-dejavu-core \
        fonts-inter \
        python3 \
        git \
        ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# xvfb, xauth, x11-utils, imagemagick, xdotool and the fonts are for the
# headless UI screenshots (tools/ui-shot.sh).

# Bake the pinned toolchain (rust-toolchain.toml pins 1.92.0) into the image.
RUN rustup toolchain install 1.92.0 --profile minimal --component rustfmt --component clippy \
    && rustup default 1.92.0

# Run as a non-root user. Some workspace tests rely on Unix permission checks
# (e.g. read-only directories must reject writes), which root silently bypasses.
RUN useradd --create-home --uid 1000 dev \
    && chown -R dev:dev /usr/local/cargo /usr/local/rustup \
    && mkdir -p /target && chown dev:dev /target \
    && git config --system --add safe.directory '*'

USER dev
ENV HOME=/home/dev

WORKDIR /work
CMD ["cargo", "build", "--workspace", "--all-targets"]
