gstreamer-snapcast-rs: Snapcast multiroom audio as GStreamer elements
=====================================================================

  snapsink              GStreamer sink: PCM in, a Snapcast stream out (server)
  snapsrc               GStreamer source: a Snapcast stream in, timestamped PCM out
  gst-snapcast-daemon   the shared Snapcast server of a user: snapsink starts it, so
                        that pipelines in several processes share one server

Contents
  lib/gstreamer-1.0/    libgstsnapsink.so, libgstsnapsrc.so
  bin/                  gst-snapcast-daemon
  install.sh            installs them (see ./install.sh --help)
  fetch-snapweb.sh      downloads Snapweb, the web UI (for snapsink http-port/snapweb-dir)
  LICENSE               GPL-3.0

Install (no root):      ./install.sh
  plugins   ~/.local/share/gstreamer-1.0/plugins
  daemon    ~/.local/bin   (must be on PATH, or set GST_SNAPCAST_DAEMON)
System-wide:            sudo ./install.sh --system
Remove:                 ./install.sh --uninstall   (add --system for a system install)

Needs GStreamer 1.22 or newer (with gst-plugins-base) and a glibc at least as new as
the build machine's; BUILD-INFO says which (the CPU architecture is in the file name).
Nothing else: no C library beyond GStreamer.

Try it
  gst-inspect-1.0 snapsink
  gst-launch-1.0 audiotestsrc is-live=true ! audioconvert ! snapsink         # sender
  gst-launch-1.0 snapsrc ! audioconvert ! autoaudiosink                      # receiver

Documentation, paths and troubleshooting: https://github.com/aanno/gstreamer-snapcast-rs
