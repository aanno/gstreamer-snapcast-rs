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
        # alsasrc/alsasink and aplay/arecord/amixer for the snd-aloop timing tests
        gstreamer1.0-alsa \
        alsa-utils \
        gstreamer1.0-pipewire \
        # snapcast-rs: optional opus codec; ALSA for snapclient-rs (cpal)
        libopus-dev \
        libasound2-dev \
        # pw-cli and other
        pipewire-audio \
        # Reference C++ Snapcast (interop testing only, never linked). Debian
        # trixie ships 0.31.0; upstream (github.com/snapcast/snapcast) is at 0.34.0.
        snapserver \
        snapclient \
        # Avahi's dns_sd compat library: snapcast-rs's own snapserver-rs (astro-dnssd)
        # needs it to build. Our plugins use the pure-Rust mdns-sd instead.
        libavahi-compat-libdnssd-dev \
        # DLNA/UPnP PoC dependencies (dms, gmrender-resurrect)
        golang libupnp-dev libupnp-doc \
    && apt-get clean \
    && rm -rf /var/lib/apt/lists/*

# Mount point of the Claude Code config volume (see devcontainer.json "mounts"). It must
# exist and belong to vscode, so that Podman copies this ownership into the new, empty
# named volume; otherwise the volume would be owned by root.
RUN mkdir -p /home/vscode/.claude && chown vscode:vscode /home/vscode/.claude
