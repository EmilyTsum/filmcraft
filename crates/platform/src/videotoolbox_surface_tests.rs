use super::*;

fn shared_frame(surface: Arc<VtSurface>) -> VideoFrame {
    VideoFrame {
        width: surface.w as u32,
        height: surface.h as u32,
        data: PixelData::Gpu(GpuPixels::new(surface.clone(), surface.layout.chroma, surface.layout.bits)),
        color: surface.layout.color,
        par: (1, 1),
        pts: filmcraft_time::Tick::ZERO,
    }
}

fn frame_plan(frame: &VideoFrame) -> filmcraft_render::plan::FramePlan {
    filmcraft_render::plan::FramePlan::Layers {
        width: frame.width as usize,
        height: frame.height as usize,
        layers: vec![filmcraft_render::plan::PlanLayer::new(Arc::new(frame.clone()), Default::default(), 1.0, filmcraft_render::Blend::Normal)],
    }
}

/// Use the product export path with the same retained frame that preview consumes.
fn export_frame(frame: &VideoFrame, path: &str) -> filmcraft_export::Result<filmcraft_export::Report> {
    use filmcraft_media::{FrameRequest, Generator, MediaInfo, MediaSource, generators::GeneratorSource};
    use filmcraft_project::{ItemKind, Label, MediaClip, MediaRef, Project, SequenceSettings, TrackKind};
    use filmcraft_time::{FrameRate, Tick, TimeRange};
    struct Source {
        frame: Arc<VideoFrame>,
        info: MediaInfo,
    }
    impl MediaSource for Source {
        fn info(&self) -> &MediaInfo {
            &self.info
        }
        fn video_frame(&self, _: FrameRequest) -> filmcraft_media::Result<Arc<VideoFrame>> {
            Ok(self.frame.clone())
        }
        fn audio(&self, _: i64, _: usize, _: u32) -> filmcraft_media::Result<filmcraft_frame::AudioBuffer> {
            Err(filmcraft_media::MediaError::NoStream("audio"))
        }
    }
    let rate = FrameRate::FPS_24;
    let generator = GeneratorSource::new(Generator::ColorMatte { color: [1.0; 4] }, frame.width, frame.height, rate, rate.tick_of(1));
    let mut project = Project::new("native readback failure");
    let item = project.add_item(
        "surface",
        Label::Iris,
        ItemKind::Media(MediaClip {
            media: MediaRef::Generator(generator.generator.clone()),
            info: generator.info().clone(),
            interpret: Default::default(),
            mark_in: None,
            mark_out: None,
            markers: vec![],
            offline: false,
            proxy: None,
            identity: None,
        }),
        None,
    );
    let seq =
        project.new_sequence("one frame", SequenceSettings { width: frame.width, height: frame.height, frame_rate: rate, ..Default::default() }, 1, 0, None);
    let clip = project.make_track_item(item, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, rate.tick_of(1)), rate).unwrap();
    project.sequence_mut(seq).unwrap().video_tracks[0].items.push(clip);
    let mut sources = filmcraft_render::SourceMap::default();
    sources.0.insert(item, Arc::new(Source { frame: Arc::new(frame.clone()), info: generator.info().clone() }));
    let settings = filmcraft_export::ExportSettings {
        format: filmcraft_export::Format::PngSequence,
        path: path.into(),
        include_audio: false,
        gpu_rendering: filmcraft_export::GpuRendering::Off,
        ..Default::default()
    };
    filmcraft_export::export(&Arc::new(project), seq, &settings, &sources, &Default::default())
}

#[test]
fn shared_readback_preserves_terminal_causes_and_recovers_retryable_preview_and_export() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/shared-surface-tests");
    std::fs::create_dir_all(&root).unwrap();
    for chroma in [Chroma::C420, Chroma::C422] {
        for bits in [8, 10] {
            let terminal = Arc::new(surface_format(bits, chroma, 5));
            let frame = shared_frame(terminal.clone());
            let path = root.join(format!("terminal-{}.png", terminal.id));
            let path = path.to_str().unwrap();
            let output = filmcraft_export::image_sequence_path(path, 0, 1);
            std::fs::write(&output, b"previous export").unwrap();
            for _ in 0..2 {
                let _ = filmcraft_frame::pending::take();
                assert_eq!(frame.to_rgba8().unwrap_err(), "decoded picture planes are smaller than the picture");
                assert!(!filmcraft_frame::pending::take(), "terminal error must not request a retry");
                let error = filmcraft_render::plan::execute_cpu(&frame_plan(&frame)).unwrap_err();
                assert!(error.contains("decoded picture planes are smaller than the picture"));
                let error = export_frame(&frame, path).unwrap_err().to_string();
                assert!(error.contains("decoded picture planes are smaller than the picture"), "{error}");
                assert_eq!(std::fs::read(&output).unwrap(), b"previous export");
                assert!(terminal.pb.lock().unwrap().is_some());
            }
            std::fs::remove_file(&output).unwrap();

            let surface = Arc::new(surface_format(bits, chroma, 4));
            let frame = shared_frame(surface.clone());
            let path = root.join(format!("retryable-{}.png", surface.id));
            let path = path.to_str().unwrap();
            let output = filmcraft_export::image_sequence_path(path, 0, 1);
            std::fs::write(&output, b"previous export").unwrap();
            // A held surface lock is demonstrably temporary. This does not classify a driver error.
            let held = surface.pb.lock().unwrap();
            for _ in 0..2 {
                let _ = filmcraft_frame::pending::take();
                assert!(frame.to_rgba8().unwrap_err().contains("surface is in use"));
                assert!(filmcraft_frame::pending::take());
                assert!(filmcraft_render::plan::execute_cpu(&frame_plan(&frame)).unwrap_err().contains("surface is in use"));
                assert!(export_frame(&frame, path).unwrap_err().to_string().contains("surface is in use"));
                assert_eq!(std::fs::read(&output).unwrap(), b"previous export");
                assert!(held.is_some());
            }
            drop(held);
            let _ = filmcraft_frame::pending::take();
            assert_eq!(frame.to_rgba8().unwrap().len(), 4 * 4 * 4);
            assert!(!filmcraft_frame::pending::take());
            assert_eq!(filmcraft_render::plan::execute_cpu(&frame_plan(&frame)).unwrap().px.len(), 4 * 4 * 4);
            let report = export_frame(&frame, path).expect("same frame recovers after releasing the lock");
            assert_eq!(report.frames, 1);
            assert_ne!(std::fs::read(&output).unwrap(), b"previous export");
            std::fs::remove_file(output).unwrap();
            eprintln!("readback preview/export: {chroma:?} {bits}-bit terminal repeat + retryable recovery verified");
        }
    }
}

fn metal_compositor() -> Option<filmcraft_gpu::GpuCompositor> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor { backends: wgpu::Backends::METAL, ..wgpu::InstanceDescriptor::new_without_display_handle() });
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())).ok()?;
    let (device, queue) = pollster::block_on(adapter.request_device(&filmcraft_gpu::device_descriptor(&adapter))).ok()?;
    Some(filmcraft_gpu::GpuCompositor::new(&device, &queue))
}

#[test]
fn shared_metal_import_covers_nv12_p010_nv16_p210_and_eviction_lifetimes() {
    filmcraft_gpu::set_surface_importer(import_metal_surface);
    let Some(mut compositor) = metal_compositor() else {
        assert!(std::env::var_os("FILMCRAFT_REQUIRE_VT").is_none(), "required Metal device is unavailable");
        eprintln!("SKIPPED: no Metal adapter");
        return;
    };
    for chroma in [Chroma::C420, Chroma::C422] {
        for bits in [8, 10] {
            let surface = Arc::new(surface_format(bits, chroma, 4));
            fill_spatial_pattern(&surface);
            let frame = shared_frame(surface.clone());
            let plan = frame_plan(&frame);
            let uploaded = compositor.uploaded_bytes;
            compositor.composite(&plan).expect("direct native import");
            assert_eq!(compositor.uploaded_bytes, uploaded, "{chroma:?} {bits}-bit must import without CPU upload");
            let (_, _, native) = compositor.read_output().expect("native output");
            assert!(surface.u8_planes.lock().unwrap().is_none());
            assert!(surface.u16_planes.lock().unwrap().is_none());
            surface.evict_to_cpu().unwrap();
            assert!(surface.pb.lock().unwrap().is_none());
            compositor.composite(&plan).expect("cached Metal textures retain their CoreVideo owners after eviction");
            assert_eq!(compositor.uploaded_bytes, uploaded);
            assert_eq!(compositor.read_output().unwrap().2, native);

            let cpu = frame.cpu().unwrap().into_owned();
            compositor.composite(&frame_plan(&cpu)).unwrap();
            assert!(compositor.uploaded_bytes > uploaded, "positive CPU-upload control");
            let cpu_output = compositor.read_output().unwrap().2;
            let error = native.iter().zip(&cpu_output).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
            assert!(error <= 1, "{chroma:?} {bits}-bit native/CPU max error {error}");

            // A fresh device cannot reimport an evicted buffer; it must use the retained CPU data.
            let mut replacement = metal_compositor().expect("replacement Metal device");
            replacement.composite(&plan).expect("CPU fallback after device replacement");
            assert!(replacement.uploaded_bytes > 0);
            assert_eq!(replacement.read_output().unwrap().2, cpu_output);
            eprintln!("Metal: {chroma:?} {bits}-bit direct CPU bytes=0; max error={error}; eviction/device replacement verified");
        }
    }
}

// Vary both axes so a 4:2:2 plane sampled as 4:2:0 cannot pass parity by accident.
fn fill_spatial_pattern(surface: &VtSurface) {
    let held = surface.pb.lock().unwrap();
    let pb = held.as_ref().unwrap();
    // SAFETY: the test exclusively holds the live buffer before importing or reading it.
    assert_eq!(unsafe { CVPixelBufferLockBaseAddress(pb, CVPixelBufferLockFlags::empty()) }, 0);
    let sample_bytes = if surface.layout.bits == 8 { 1 } else { 2 };
    for plane in 0..2 {
        let stride = CVPixelBufferGetBytesPerRowOfPlane(pb, plane);
        let height = CVPixelBufferGetHeightOfPlane(pb, plane);
        let width = CVPixelBufferGetWidthOfPlane(pb, plane) * if plane == 0 { 1 } else { 2 };
        let base = CVPixelBufferGetBaseAddressOfPlane(pb, plane) as *mut u8;
        assert!(!base.is_null());
        // SAFETY: CoreVideo maps stride * height bytes and the buffer is locked exclusively.
        let bytes = unsafe { std::slice::from_raw_parts_mut(base, stride * height) };
        for y in 0..height {
            for x in 0..width {
                let value = if plane == 0 { 48 + x * 15 + y * 9 } else { 96 + (x % 2) * 32 + (x / 2) * 15 + y * 9 };
                let offset = y * stride + x * sample_bytes;
                if sample_bytes == 1 {
                    bytes[offset] = value as u8;
                } else {
                    bytes[offset..offset + 2].copy_from_slice(&((value as u16) << 8).to_ne_bytes());
                }
            }
        }
    }
    // SAFETY: matches the successful lock above, before the buffer is shared.
    unsafe { CVPixelBufferUnlockBaseAddress(pb, CVPixelBufferLockFlags::empty()) };
}

fn test_layout(bits: u32) -> Layout {
    layout_format(bits, Chroma::C420)
}

fn layout_format(bits: u32, chroma: Chroma) -> Layout {
    Layout {
        pixel_format: match (bits, chroma) {
            (8, Chroma::C420) => NV12_VIDEO,
            (8, _) => NV16_VIDEO,
            (_, Chroma::C420) => P010_VIDEO,
            _ => P210_VIDEO,
        },
        coded: (4, 4),
        crop: (0, 0, 4, 4),
        chroma,
        bits,
        color: filmcraft_color::ColorInfo::REC709,
        par: (1, 1),
        alpha: false,
    }
}

fn buffer(bits: u32) -> CFRetained<CVImageBuffer> {
    buffer_format(bits, Chroma::C420)
}

fn buffer_format(bits: u32, chroma: Chroma) -> CFRetained<CVImageBuffer> {
    let mut raw = std::ptr::null_mut();
    let io = CFDictionary::<CFString, CFType>::from_slices(&[], &[]);
    // SAFETY: immutable CoreVideo framework constants.
    let attrs = unsafe {
        CFDictionary::<CFString, CFType>::from_slices(
            &[objc2_core_video::kCVPixelBufferIOSurfacePropertiesKey, kCVPixelBufferMetalCompatibilityKey],
            &[io.as_ref(), CFBoolean::new(true).as_ref()],
        )
    };
    // SAFETY: fixed valid dimensions/format and a valid output pointer; no borrowed storage.
    let status =
        unsafe { objc2_core_video::CVPixelBufferCreate(None, 4, 4, layout_format(bits, chroma).pixel_format, Some(attrs.as_ref()), NonNull::from(&mut raw)) };
    assert_eq!(status, 0);
    // SAFETY: creation succeeded and returns a retained (+1) buffer.
    let pb = unsafe { CFRetained::from_raw(NonNull::new(raw).expect("created pixel buffer")) };
    // SAFETY: the pixel buffer is live and uniquely held while its test pixels are initialized.
    assert_eq!(unsafe { CVPixelBufferLockBaseAddress(&pb, CVPixelBufferLockFlags::empty()) }, 0);
    for i in 0..2 {
        let base = CVPixelBufferGetBaseAddressOfPlane(&pb, i) as *mut u8;
        let len = CVPixelBufferGetBytesPerRowOfPlane(&pb, i) * CVPixelBufferGetHeightOfPlane(&pb, i);
        assert!(!base.is_null());
        // SAFETY: CoreVideo maps this plane's stride * height bytes; the exclusive buffer is locked.
        let bytes = unsafe { std::slice::from_raw_parts_mut(base, len) };
        if bits == 8 {
            bytes.fill(0x25 + i as u8 * 0x10);
        } else {
            for word in bytes.as_chunks_mut::<2>().0 {
                word.copy_from_slice(&(0x2500u16 + i as u16 * 0x1000).to_ne_bytes());
            }
        }
    }
    // SAFETY: matches the successful lock above, before the buffer is shared.
    unsafe { CVPixelBufferUnlockBaseAddress(&pb, CVPixelBufferLockFlags::empty()) };
    pb
}

fn surface(bits: u32, w: usize) -> VtSurface {
    surface_format(bits, Chroma::C420, w)
}

fn surface_format(bits: u32, chroma: Chroma, w: usize) -> VtSurface {
    VtSurface {
        id: NEXT_SURFACE_ID.fetch_add(1, Ordering::Relaxed),
        native_lock_failures: AtomicU64::new(0),
        pb: Mutex::new(Some(buffer_format(bits, chroma))),
        layout: layout_format(bits, chroma),
        ox: 0,
        oy: 0,
        w,
        h: 4,
        u8_planes: Mutex::new(None),
        u16_planes: Mutex::new(None),
    }
}

fn shared(bits: u32) -> Shared {
    Shared { layout: Mutex::new(test_layout(bits)), out: Mutex::new(Vec::new()), surfaces: Mutex::new(VecDeque::new()) }
}

#[test]
fn failed_surface_eviction_preserves_the_buffer_for_repeated_reads_and_recovery() {
    for bits in [8, 10] {
        // A real CoreVideo buffer with a deliberately oversized requested view forces the actual
        // plane-copy validation to fail without depending on a driver lock failure.
        let mut s = surface(bits, 5);
        for _ in 0..2 {
            assert_eq!(s.evict_to_cpu().unwrap_err(), "decoded picture planes are smaller than the picture");
            if bits == 8 {
                assert!(s.read_planes_u8().is_err());
            } else {
                assert!(s.read_planes_u16().is_err());
            }
            assert!(s.pb.lock().unwrap().is_some(), "{bits}-bit original must survive a failed copy");
            assert!(s.u8_planes.lock().unwrap().is_none());
            assert!(s.u16_planes.lock().unwrap().is_none());
        }
        s.w = 4;
        s.evict_to_cpu().expect("the same buffer must remain readable after correcting the view");
        assert!(s.pb.lock().unwrap().is_none());
        s.evict_to_cpu().expect("repeat eviction uses the cached planes");
        if bits == 8 {
            let [y, uv] = s.read_planes_u8().expect("CPU planes survive releasing the buffer");
            assert_eq!(y.as_slice(), &[0x25; 16]);
            assert_eq!(uv.as_slice(), &[0x35; 8]);
        } else {
            let [y, uv] = s.read_planes_u16().expect("CPU planes survive releasing the buffer");
            assert_eq!(y.as_slice(), &[0x2500; 16]);
            assert_eq!(uv.as_slice(), &[0x3500; 8]);
        }
    }
}

#[test]
fn failed_surface_eviction_rejects_new_frames_without_losing_tracking() {
    for bits in [8, 10] {
        let state = shared(bits);
        let held: Vec<_> = (0..MAX_RETAINED_SURFACES).map(|i| Arc::new(surface(bits, if i == 0 { 5 } else { 4 }))).collect();
        for s in &held {
            state.track_surface(s).unwrap();
        }
        let pb = buffer(bits);
        for _ in 0..2 {
            let error = copy_out(&pb, &test_layout(bits), &state).unwrap_err();
            assert!(error.contains("cannot evict retained VideoToolbox surface"), "{error}");
            assert!(error.contains("decoded picture planes are smaller than the picture"), "{error}");
            let q = state.surfaces.lock().unwrap();
            assert_eq!(q.len(), MAX_RETAINED_SURFACES);
            assert!(Weak::ptr_eq(q.front().unwrap(), &Arc::downgrade(&held[0])));
            assert!(held.iter().all(|s| s.pb.lock().unwrap().is_some()));
        }
    }
}

#[test]
fn successful_surface_eviction_keeps_the_retention_limit_and_original_pixels() {
    for bits in [8, 10] {
        let state = shared(bits);
        let mut held = Vec::new();
        for _ in 0..MAX_RETAINED_SURFACES * 3 {
            let s = Arc::new(surface(bits, 4));
            state.track_surface(&s).unwrap();
            held.push(s);
            assert!(state.surfaces.lock().unwrap().len() <= MAX_RETAINED_SURFACES);
            assert_eq!(held.iter().filter(|s| s.pb.lock().unwrap().is_some()).count(), held.len().min(MAX_RETAINED_SURFACES));
        }
        for s in &held {
            if bits == 8 {
                let [y, uv] = s.read_planes_u8().unwrap();
                assert_eq!(y.as_slice(), &[0x25; 16]);
                assert_eq!(uv.as_slice(), &[0x35; 8]);
            } else {
                let [y, uv] = s.read_planes_u16().unwrap();
                assert_eq!(y.as_slice(), &[0x2500; 16]);
                assert_eq!(uv.as_slice(), &[0x3500; 8]);
            }
        }
    }
}

#[test]
fn native_lock_failure_policy_is_bounded_and_keeps_the_status_code() {
    for bits in [8, 10] {
        let surface = surface(bits, 4);
        let status = objc2_core_video::kCVReturnError;
        let cause = format!("cannot lock the decoded picture (CVReturn {status})");
        for _ in 0..2 {
            assert_eq!(surface.classify_readback_error(PlaneReadbackError::Lock(status)), CpuReadbackError::Retryable(cause.clone()));
            assert!(surface.pb.lock().unwrap().is_some());
        }
        // Successful real CoreVideo readback resets the policy counter.
        surface.download_classified().unwrap();
        assert_eq!(surface.native_lock_failures.load(Ordering::Relaxed), 0);
        for _ in 0..2 {
            assert!(matches!(surface.classify_readback_error(PlaneReadbackError::Lock(status)), CpuReadbackError::Retryable(_)));
        }
        assert_eq!(surface.classify_readback_error(PlaneReadbackError::Lock(status)), CpuReadbackError::Terminal(cause));
        assert!(matches!(surface.classify_readback_error(PlaneReadbackError::Lock(objc2_core_video::kCVReturnInvalidArgument)), CpuReadbackError::Terminal(_)));
    }
}
