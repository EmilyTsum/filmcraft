# macOS VideoToolbox formats

The macOS backend uses hardware-required VideoToolbox sessions. A codec name or chip generation
is not sufficient: session creation checks the actual profile, dimensions and output layout.
Ordinary unsupported streams keep the software decoder. A failed hardware decoder replays from
the latest random access sample in software; ProRes and JPEG restart on every sample.
HEVC alpha has no equivalent software decoder. Without a VideoToolbox session (other platforms,
hardware decoding Off, Main 10 alpha) it opens in software showing its opaque base layer, with a
logged warning; a running hardware alpha session that fails mid-stream reports an error rather
than silently turning opaque.

## Formats

| Format | Decode | Encode |
|---|---|---|
| H.264 Baseline / Main / High, 8-bit 4:2:0 | hardware where supported | hardware, opt-in |
| H.264 10-bit / 4:2:2 | hardware where supported | not offered by Apple's enumerated H.264 encoder on the validated Mac |
| HEVC Main, 8-bit 4:2:0 | hardware where supported | hardware |
| HEVC Main 10, 10-bit 4:2:0 | hardware where supported | hardware, SDR / PQ / HLG |
| HEVC Main 4:2:2 10, 10-bit 4:2:2 | hardware where supported | hardware, SDR / PQ / HLG |
| H.264 / HEVC 8/10-bit 4:4:4 | requested without reducing chroma; session may decline | not offered by the validated encoders |
| HEVC with alpha | hardware, 8-bit SDR, straight RGBA | hardware, Main 8-bit SDR |
| ProRes 422 Proxy / LT / Standard / HQ | hardware, 10-bit 4:2:2 | hardware, opt-in, SDR / PQ / HLG |
| ProRes 4444 / 4444 XQ | hardware, planar 12-bit 4:4:4 plus alpha when present | hardware, opt-in, SDR / PQ / HLG, optional alpha |
| VP9 profile 0 / 2 | supplemental hardware decoder, 8/10-bit 4:2:0 | no VideoToolbox encoder |
| AV1 main | supplemental hardware decoder, 8/10-bit 4:2:0 | no VideoToolbox encoder |
| Motion JPEG (`jpeg`) | VideoToolbox native, straight RGBA8 (HW requested, status unconfirmed) | hardware, opt-in |

ProRes RAW is out of scope. VideoToolbox's depth/disparity and immersive/spatial-video APIs are not
ordinary FilmCraft video formats. This table does not promise a hardware implementation for every
profile on every Mac. 12-bit HEVC, AV1 high/professional profiles, 12-bit VP9, and field-coded H.264
are declined by these paths; they are never silently converted to 8-bit or 4:2:0.

On the tested Apple M5/macOS system, the OS can create an encoder for HEVC Main 10 with alpha but
refuses the corresponding decoder even without output-pixel-format restrictions. This combination
is rejected explicitly in the export adapter and low-level encoder; use ProRes 4444/XQ for HDR
with alpha. No 8-bit substitution is made. With hardware decoding Off, HEVC alpha opens opaque in
software.

JPEG needs special care: Apple's default JPEG component ignores the hardware-required flag and
selects software. We enumerate a component marked `IsHardwareAccelerated`, select its exact
`EncoderID`, and check that the active encoder ID matches when the component does not implement
`UsingHardwareAcceleratedVideoEncoder`. Other encoders require that property to confirm hardware. The JPEG decoder also omits the hardware
status property; its native path requests hardware but is not counted as confirmed hardware in
`perf.stats`. Actual JPEG hardware decoding was not independently confirmed.

## Export settings

- `hardwareEncoding: "auto"` opts H.264, ProRes and Motion JPEG into hardware. Off keeps the existing
  reproducible software encoders. Choosing HEVC is itself the hardware opt-in.
- `hevcProfile`: `auto` (the default, Main for SDR and Main 10 for HDR), `main`, `main10`, or
  `main42210`. Explicit choices are available in Export and `file.exportMedia`; invalid values and
  an 8-bit choice for HDR are errors. Explicit profile selection currently requires macOS.
- `proresProfile`: `proxy`, `lt`, `standard`, `hq`, `4444`, `4444xq`.
- `alpha: true` keeps straight alpha for ProRes 4444/XQ and macOS HEVC Main. HDR rendering preserves
  a separate alpha channel and unpremultiplies colour before transfer encoding.

Decoded biplanar frames retain their CoreVideo surface for direct Metal texture import; CPU consumers
materialise their planes on demand. Retention is bounded, and GPU or format incompatibility uses
the CPU upload path. Packed alpha formats continue through CPU conversion.

The review integration uses the shared `GpuSurface` / `GpuPixels` / `PixelData::Gpu`
boundary from #1021 at `7344ff1b0db36499c63b592deb8888c5e81dec49`, including #1019's
fallible CPU conversions. It does not depend on the #1114 browser adapter. The shared importer
accepts explicit NV12/P010 (4:2:0) and NV16/P210 (4:2:2) layouts, checking texture format,
dimensions and surface size before caching an import.

Eviction releases a CoreVideo buffer only after its CPU planes have been copied successfully.
A failed copy retains the original buffer and tracking entry and rejects the newly decoded
picture through the decoder-error/software-replay path. At most eight decoder surface owners
retain native buffers; imported GPU textures independently retain CoreVideo resources until
GPU use/cache eviction completes. The shared compositor source cache remains bounded at 24
entries, as in the pinned Windows implementation; the eight-owner bound is not a bound on
all outstanding GPU texture holders.

`CpuReadbackError` distinguishes retryable and terminal failures without changing existing
adapters' `download() -> Result<PixelData, String>` implementation contract. The default
`download_classified()` treats those adapters' errors as terminal. Successful CPU data and
terminal causes are shared across frame clones; retryable failures never become cached pixels
or permanent CPU errors. Preview workers discard retryable job results and request the picture
again. Export returns an error before writing that frame; a later export can retry the same
retained picture.

For the macOS adapter, surface-mutex contention is retryable; invalid/missing planes and layout
validation failures are terminal. Native lock failures retain the numeric CVReturn cause.
Invalid argument/format/size codes are terminal immediately. Other native lock failures permit
two later explicit attempts, then become terminal with their original cause if the third attempt
fails. A successful native read resets this counter. This is a bounded recovery policy; it does
not assert that any particular driver's error is transient. There is no automatic loop inside
a readback request.

Synthetic IOSurface-backed CoreVideo tests exercise all four formats, repeated CPU-preview and
PNG-export failures without overwriting previous output, recovery after releasing a held surface
lock, successful bounded eviction, direct Metal import, texture lifetime after decoder-buffer
eviction, and CPU fallback with a new Metal device. A separate injected-status test checks the
native-error policy. These tests do not reproduce a physical driver lock failure or GPU reset.

10-bit input uses P010/P210, with significant bits shifted into 16-bit CoreVideo storage. ProRes
4444 uses packed little-endian AYUV16, preserving alpha separately from video-range YUV. The current
export pipeline converts YUV to 10-bit values (including ProRes 4444); its 12-bit decoded output
must not be mistaken for an end-to-end 12-bit source/export pipeline. PQ/HLG colour is written to
the stream and sample entry; PQ also carries mastering-display/content-light metadata. Export
verifies the HEVC header's bit depth, chroma and alpha against the requested configuration.

Source references: Apple's Xcode SDK headers `VTCompressionProperties.h`, `VTDecompressionProperties.h`,
`VTVideoEncoderList.h`, `CMFormatDescription.h` and `CVPixelBuffer.h`. The VP9 and AV1 integration
and its parity tests adapt the existing contributions in [#2180](https://github.com/storytold/filmcraft/pull/2180)
and [#2608](https://github.com/storytold/filmcraft/pull/2608).

## Reproducing validation

```sh
# An absent device implementation skips only that hardware case.
cargo test -p filmcraft-platform --test vt_formats -- --nocapture
# Require all supported profile cases on a suitable Mac; no silent skips.
FILMCRAFT_REQUIRE_VT=1 cargo test -p filmcraft-platform --test vt_formats profile_round_trips -- --nocapture
cargo test -p filmcraft-platform --test videotoolbox --test hardware_encode
# Optional private/local media. Never commit this media.
FILMCRAFT_TEST_PRORES=/absolute/path/to/a-short-prores.mov \
  cargo test -p filmcraft-platform --test vt_formats local_prores_decode_and_seek -- --ignored --nocapture
```

`vt_formats` generates its own pictures and movies under `target/fixtures/platform/`. It checks six
ProRes profiles, HEVC Main/Main 10/Main 4:2:2 10 in SDR/PQ/HLG, supported alpha, JPEG, real export
jobs, seeks, forced software fallback, malformed samples and invalid inputs. FFmpeg/ffprobe are
external oracles only: independent decoding, frame counts, pixel formats, colour and HDR metadata.
The VP9/AV1 tests compare hardware output with FilmCraft's software decoder at 360p, 1080p and 4K.

Local evidence (Apple M5, 2026-10-21, development profile): a local 4K60 ProRes LT
capture, 120 frames copied without transcoding from 60 seconds into the movie, decoded and sought
successfully. Against FilmCraft software decode, worst luma/chroma PSNR was 79.28 dB, maximum
10-bit code difference 1. Hardware decoding, including compressed sample copies and CPU readback,
ran at 82.9 fps in one integrated run with workspace tests active concurrently. This is a short
segment under concurrent load, not a dedicated benchmark or full-movie/editor playback performance.
No private video, still, title, path or extracted frame is included in the repository.


## Pinned API review (2026-10-22)

The review branch adapts the macOS feature delta from #2657 head
`2c33590781d5236949d52aa780d8227f5d76b794` onto the published #1021 snapshot
`7344ff1b0db36499c63b592deb8888c5e81dec49`. It does not include #1114's browser
adapter or change #2657's dependency base. This is an API review tree, not a current-main
integration or an agreement on merge order.

The shared GPU API has additive classified readback and explicit 4:2:2 plane validation.
Terminal causes are cached across clones; retryable failures leave the surface and preview
plan available. Synthetic CoreVideo tests cover NV12/P010/NV16/P210 with spatially varying
planes, direct Metal import without CPU uploads, pixel comparison against CPU upload,
retained textures after CPU eviction, and CPU fallback on a replacement device. Plane-copy
validation and temporary ownership contention exercise repeated preview/export failure and
recovery without overwriting an existing export. Native status classification is a policy
unit test; no physical driver-lock failure or driver reset is claimed.
