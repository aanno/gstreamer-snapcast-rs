# Rust devcontainer (Debian trixie) + everything needed to build and run
# GStreamer plugins written in Rust, and to build the snapcast-rs submodule.
FROM mcr.microsoft.com/devcontainers/rust:2-1-trixie

RUN apt-get update \
    && export DEBIAN_FRONTEND=noninteractive \
    && apt-get install -y --no-install-recommends \
        pkg-config \
        # GStreamer development headers (gstreamer-rs bindings link against these)
        libgstreamer1.0-dev \
        libgstreamer-plugins-base1.0-dev \
        # gst-launch-1.0 / gst-inspect-1.0 and common elements for test pipelines
        gstreamer1.0-tools \
        gstreamer1.0-plugins-base \
        gstreamer1.0-plugins-good \
        # snapcast-rs: optional opus codec; ALSA for snapclient-rs (cpal)
        libopus-dev \
        libasound2-dev \
    && apt-get clean \
    && rm -rf /var/lib/apt/lists/*
