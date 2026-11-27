# DLNA/UPNP-AV Implementations

## Media Server

* [dms](https://github.com/anacrolix/dms)
  + golang
  + ffmpeg as multimedia backend
  + my dms fork: https://github.com/aanno/dms
* [rygel](https://gnome.pages.gitlab.gnome.org/rygel/index.html)
  + GNOME, gobject based
  + C lang
  + also provides an renderer (but no controller)

### Not tried so far

* [vuio](https://github.com/vuiodev/vuio)
  + rust
  + with web ui
* [RustCast](https://github.com/celoficial/RustCast)
  + rust
  + old, maybe unmaintained

### Stripped down media servers (casters and the like)

* [go2tv](https://github.com/alexballas/go2tv)
  + go lang
  + GUI based on https://github.com/go-gl/glfw (https://www.glfw.org/)
  + many dependencies on (deprecated) X11 (comming from glfw?!?)

## Media Renderer

* [gmrender-resurrect](https://github.com/hzeller/gmrender-resurrect)
  + C lang
  + gstreamer as multimedia backend
  + my gmrender fork: https://github.com/aanno/gmrender-resurrect
* [rygel](https://gnome.pages.gitlab.gnome.org/rygel/index.html)

## Media Controller

There _is_ a shortage of open source DLNA _controllers_.

* [tutanaut](https://www.tutonaut.de/update-mein-upnp-control-point-1-3/)
  + python
  + vibe coding
* [ezUpnp](https://www.eezupnp.de/)
  + PD, but _not_ open source
  + java and eclipse RCP based
  + no source code

### Controllers _not_ working

* [UpPlay](https://www.lesbonscomptes.com/upplay/index.html)
  + C++ lang
  + uses the libupnpp library
  + AppImage not working on Fedora 45
  + compilation not tried so far
* [muspnp](https://github.com/phpbg/muspnp/)
  + JS lang
  + could not build it on Fedora 45

## References

* https://github.com/hzeller/gmrender-resurrect/wiki/Compatibility



