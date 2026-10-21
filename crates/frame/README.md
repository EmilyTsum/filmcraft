# filmcraft-frame

Portable CPU/GPU frame storage and color metadata. The shared GPU boundary originates in
PR #1019 / #1021; this review uses pinned #1021 `7344ff1b0db36499c63b592deb8888c5e81dec49`.

`GpuSurface::download()` remains source-compatible for existing adapters. The additive
`download_classified()` returns `CpuReadbackError::Retryable` or `Terminal`; its default maps
existing download failures to terminal errors. `GpuPixels` serializes readback across clones,
shares successful pixels or terminal causes, and leaves retryable errors uncached. The original
cause remains available on repeated CPU/render/export requests. `pending::take()` lets preview
workers avoid permanently caching retryable results. GPU presentation also keeps the original
plan available after retryable CPU fallback failures. No failure produces successful blank pixels.

The GPU importer accepts NV12/P010 (4:2:0) and NV16/P210 (4:2:2) with validated texture layouts.
Native adapter retry policies and resource lifetimes remain platform responsibilities.
