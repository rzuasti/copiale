# Libraries GPUI dlopens at runtime on Linux.
pkgs:
with pkgs; [
  wayland
  libxkbcommon
  vulkan-loader
  libGL
  libx11
  libxcursor
  libxi
  libxrandr
  libxcb
  fontconfig
  freetype
  dbus
]
