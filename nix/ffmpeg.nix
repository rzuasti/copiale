# FFmpeg as the video helper needs it (`copiale --video-frame`,
# `--video-play`): only the decoding and conversion libraries, LGPL, no
# network, devices, filters or programs. H.264, HEVC, VP9 and AAC decoders
# are FFmpeg's own; AV1 comes from dav1d.
{ffmpeg-headless}:
(ffmpeg-headless.override {
  withHeadlessDeps = false;
  withGPL = false;
  withDav1d = true;
  withZlib = true; # compressed MOV headers
  buildAvcodec = true;
  buildAvformat = true;
  buildAvutil = true;
  buildSwresample = true;
  buildSwscale = true;
}).overrideAttrs {
  # FFmpeg's test tools need the filter library; Copiale's tests cover
  # what the helper uses.
  doCheck = false;
}
