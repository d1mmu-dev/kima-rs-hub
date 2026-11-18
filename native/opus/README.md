# libopus (Opus decoding for analysis and CLAP embedding)

Symphonia, kima's in-process decoder, has no Opus codec. `kima-ml` registers the
`symphonia-adapter-libopus` decoder (MIT OR Apache-2.0), which calls libopus
through `opusic-sys` (BSD-3-Clause). With the `bundled` feature `opusic-sys`
builds libopus **1.6.1** from the source vendored in its crate (crates.io
`opusic-sys` 0.7.7, checksum pinned in `Cargo.lock`) with CMake and links it
**statically** into `kima`; no libopus shared library is needed at run time.
Divergence D-251 records the behaviour.

libopus is copyright 2001-2023 Xiph.Org, Skype Limited, Octasic, Jean-Marc
Valin, Timothy B. Terriberry, CSIRO, Gregory Maxwell, Mark Borgerding,
Erik de Castro Lopo, Mozilla, Amazon and others, under the BSD-3-Clause
licence. The Docker image carries its full `COPYING` (copied from the exact
crate source the build used) and this note in `/opt/kima/notices/libopus/`.

Build prerequisite: CMake >= 3.16 and a C compiler (the Docker `backend` stage
installs `cmake`; the CI jobs already install it for libsoxr).

Gapless handling: the adapter discards the OpusHead pre-skip from the first
decoded packet and the Ogg demuxer trims the final packet to the last page's
granule position, so a track decodes to exactly its 48 kHz length (as ffmpeg
and libsndfile do upstream). Known limits, both recorded in D-251: only mono
and stereo Opus streams (channel mapping family 0) decode; a pre-skip longer
than the first packet is only discarded from that packet.
