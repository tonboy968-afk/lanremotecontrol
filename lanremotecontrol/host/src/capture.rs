//! Windows DXGI Desktop Duplication screen capture implementation.
//!
//! # Overview
//!
//! This module implements [`ScreenCapture`] using the DXGI Desktop Duplication
//! API on Windows.  It captures the primary monitor's desktop frame as raw
//! BGRA pixel data at very low latency.
//!
//! # Platform Support
//!
//! All DXGI types are guarded with `#[cfg(windows)]` — on non-Windows
//! platforms the [`DxgiCapture::new`] function always returns
//! [`CaptureError::UnsupportedOs`].

// ─── Windows DXGI Implementation ───────────────────────────────────────────

#[cfg(windows)]
mod platform {
    use lanremotecontrol_common::capture::{CapturedFrame, CaptureError, ScreenCapture};
    use std::time::{SystemTime, UNIX_EPOCH};
    use windows::Win32::Graphics::Direct3D11::*;
    use windows::Win32::Graphics::Direct3D::*;
    use windows::Win32::Graphics::Dxgi::*;
    use windows::Win32::Graphics::Dxgi::Common::*;
    use windows::core::Interface;

    /// A DXGI-based screen capture instance for the primary monitor.
    pub struct DxgiCapture {
        device: ID3D11Device,
        context: ID3D11DeviceContext,
        duplication: IDXGIOutputDuplication,
        output_desc: DXGI_OUTPUT_DESC,
        staging_texture: Option<ID3D11Texture2D>,
    }

    // SAFETY: D3D11 device context is not thread-safe, but we serialize
    // all access through the single capture thread (the caller's thread).
    unsafe impl Send for DxgiCapture {}
    unsafe impl Sync for DxgiCapture {}

    impl DxgiCapture {
        /// Create a new DXGI capture instance for the primary monitor.
        pub fn new() -> Result<Self, CaptureError> {
            // ── 1. Create D3D11 device ────────────────────────────────────
            let device = create_d3d11_device()?;

            // ── 2. Get DXGI device ────────────────────────────────────────
            let dxgi_device: IDXGIDevice = device
                .cast()
                .map_err(|e| CaptureError::InitFailed(format!("cast to IDXGIDevice: {}", e)))?;

            // ── 3. Get DXGI adapter ───────────────────────────────────────
            let adapter: IDXGIAdapter = unsafe {
                dxgi_device
                    .GetAdapter()
                    .map_err(|e| CaptureError::InitFailed(format!("GetAdapter: {}", e)))?
            };

            // ── 4. Get first output (primary monitor) ─────────────────────
            let output: IDXGIOutput = unsafe {
                adapter
                    .EnumOutputs(0)
                    .map_err(|e| CaptureError::InitFailed(format!("EnumOutputs(0): {}", e)))?
            };

            // ── 4a. Get output description ────────────────────────────────
            let output_desc: DXGI_OUTPUT_DESC = unsafe {
                output
                    .GetDesc()
                    .map_err(|e| CaptureError::InitFailed(format!("GetDesc: {}", e)))?
            };

            // ── 4b. Query IDXGIOutput1 for DuplicateOutput ────────────────
            let output1: IDXGIOutput1 = output
                .cast()
                .map_err(|e| CaptureError::InitFailed(format!("cast to IDXGIOutput1: {}", e)))?;

            // ── 5. Create output duplication ──────────────────────────────
            let duplication: IDXGIOutputDuplication = unsafe {
                output1
                    .DuplicateOutput(&device)
                    .map_err(|e| {
                        CaptureError::InitFailed(format!("DuplicateOutput: {}", e))
                    })?
            };

            // ── 6. Get immediate context ──────────────────────────────────
            let context: ID3D11DeviceContext = unsafe {
                device
                    .GetImmediateContext()
                    .map_err(|e| CaptureError::InitFailed(format!("GetImmediateContext: {}", e)))?
            };

            Ok(Self {
                device,
                context,
                duplication,
                output_desc,
                staging_texture: None,
            })
        }

        /// Capture a single desktop frame.
        ///
        /// Blocks for up to 16 ms waiting for a new frame.  Returns the
        /// desktop frame as raw BGRA pixel data.
        pub fn capture_frame(&mut self) -> Result<CapturedFrame, CaptureError> {
            // ── Acquire next frame ──────────────────────────────────────
            let mut frame_info = DXGI_OUTDUPL_FRAME_INFO::default();
            let mut resource: Option<IDXGIResource> = None;

            unsafe {
                self.duplication
                    .AcquireNextFrame(5, &mut frame_info, &mut resource)
                    .map_err(|e| {
                        if e.code() == DXGI_ERROR_WAIT_TIMEOUT {
                            CaptureError::FrameAcquireFailed("timeout".into())
                        } else if e.code() == DXGI_ERROR_DEVICE_RESET
                            || e.code() == DXGI_ERROR_DEVICE_REMOVED
                        {
                            CaptureError::DeviceLost
                        } else {
                            CaptureError::FrameAcquireFailed(format!("{:?}", e))
                        }
                    })?;
            }

            // ── Get the texture ────────────────────────────────────────
            let texture: ID3D11Texture2D = {
                let res = resource.ok_or_else(|| {
                    CaptureError::FrameAcquireFailed(
                        "AcquireNextFrame returned None resource".into(),
                    )
                })?;
                res.cast::<ID3D11Texture2D>()
                    .map_err(|e| {
                        CaptureError::FrameAcquireFailed(format!(
                            "failed to query ID3D11Texture2D: {}",
                            e
                        ))
                    })?
            };

            // ── Get texture description ────────────────────────────────
            let tex_desc = unsafe {
                let mut desc = D3D11_TEXTURE2D_DESC::default();
                texture.GetDesc(&mut desc);
                desc
            };
            let width = tex_desc.Width;
            let height = tex_desc.Height;

            // ── Create (or reuse) staging texture ──────────────────────
            let staging = self.get_or_create_staging(width, height)?;

            // ── Copy resource to staging ───────────────────────────────
            unsafe {
                self.context
                    .CopyResource(&staging, &texture);
            }

            // ── Map staging texture ────────────────────────────────────
            let mapped = {
                let mut mapped_out = D3D11_MAPPED_SUBRESOURCE::default();
                unsafe {
                    self.context
                        .Map(
                            &staging,
                            0,
                            D3D11_MAP_READ,
                            0,
                            Some(&mut mapped_out),
                        )
                        .map_err(|e| {
                            CaptureError::FrameAcquireFailed(format!("Map failed: {}", e))
                        })?;
                }
                mapped_out
            };

            let stride = mapped.RowPitch;
            let bpp = 4u32; // BGRA
            let row_bytes = (width * bpp) as usize;
            let mut data = Vec::with_capacity((height as usize) * row_bytes);

            // Copy row by row (stride may differ from width * 4)
            let src_ptr = mapped.pData as *const u8;
            for row in 0..height as usize {
                let src_row = unsafe {
                    std::slice::from_raw_parts(src_ptr.add(row * stride as usize), row_bytes)
                };
                data.extend_from_slice(src_row);
            }

            // ── Unmap ──────────────────────────────────────────────────
            unsafe {
                self.context.Unmap(&staging, 0);
            }

            // ── Release the duplicated frame ───────────────────────────
            unsafe {
                self.duplication.ReleaseFrame().ok();
            }

            let timestamp_us = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_micros() as u64;

            Ok(CapturedFrame {
                data,
                width,
                height,
                stride: row_bytes as u32,
                timestamp_us,
            })
        }

        /// Return a human-readable description of the captured display.
        pub fn display_info(&self) -> String {
            let desc = &self.output_desc;
            let name = String::from_utf16_lossy(&desc.DeviceName);
            format!(
                "DXGI Output: {} ({}x{} @ {}x{} offset)",
                name,
                desc.DesktopCoordinates.right - desc.DesktopCoordinates.left,
                desc.DesktopCoordinates.bottom - desc.DesktopCoordinates.top,
                desc.DesktopCoordinates.left,
                desc.DesktopCoordinates.top,
            )
        }

        // ── Internal helpers ────────────────────────────────────────────

        fn get_or_create_staging(
            &mut self,
            width: u32,
            height: u32,
        ) -> Result<ID3D11Texture2D, CaptureError> {
            // Check if the cached staging matches
            if let Some(ref tex) = self.staging_texture {
                let mut desc = D3D11_TEXTURE2D_DESC::default();
                unsafe { tex.GetDesc(&mut desc); }
                if desc.Width == width && desc.Height == height {
                    return Ok(tex.clone());
                }
            }

            // Create a new staging texture
            let desc = D3D11_TEXTURE2D_DESC {
                Width: width,
                Height: height,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: 0,
            };

            let texture = unsafe {
                let mut tex: Option<ID3D11Texture2D> = None;
                self.device
                    .CreateTexture2D(
                        &desc,
                        None,
                        Some(&mut tex),
                    )
                    .map_err(|e| {
                        CaptureError::InitFailed(format!("CreateTexture2D (staging): {}", e))
                    })?;
                tex.ok_or_else(|| {
                    CaptureError::InitFailed("CreateTexture2D returned None".into())
                })?
            };

            self.staging_texture = Some(texture.clone());
            Ok(texture)
        }
    }

    impl ScreenCapture for DxgiCapture {
        fn capture_frame(&mut self) -> Result<CapturedFrame, CaptureError> {
            self.capture_frame()
        }
    }

    // ── Helper: create D3D11 device ──────────────────────────────────────

    fn create_d3d11_device() -> Result<ID3D11Device, CaptureError> {
        let flags = D3D11_CREATE_DEVICE_BGRA_SUPPORT;

        // Try hardware driver first, then fall back to WARP (software)
        let driver_types = [
            D3D_DRIVER_TYPE_HARDWARE,
            D3D_DRIVER_TYPE_WARP,
        ];

        let mut last_error = None;

        for &driver_type in &driver_types {
            let mut device_out: Option<ID3D11Device> = None;

            let result = unsafe {
                D3D11CreateDevice(
                    None,
                    driver_type,
                    windows::Win32::Foundation::HMODULE(std::ptr::null_mut()),
                    flags,
                    None,
                    D3D11_SDK_VERSION,
                    Some(&mut device_out as *mut _),
                    None,
                    None,
                )
            };

            if result.is_ok() {
                if let Some(d) = device_out {
                    return Ok(d);
                }
            }

            last_error = Some(format!("{:?}", result));
        }

        Err(CaptureError::InitFailed(format!(
            "D3D11CreateDevice failed (tried HW and WARP): {}",
            last_error.as_deref().unwrap_or("unknown error")
        )))
    }

    // ── Tests ────────────────────────────────────────────────────────────

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn test_dxgi_display_info_on_windows() {
            let capture = DxgiCapture::new();
            match capture {
                Ok(cap) => {
                    let info = cap.display_info();
                    assert!(!info.is_empty());
                    assert!(info.contains("DXGI Output:"));
                }
                Err(e) => {
                    match e {
                        CaptureError::InitFailed(_)
                        | CaptureError::NoOutput
                        | CaptureError::DeviceLost
                        | CaptureError::UnsupportedOs => {
                            // Expected in CI/headless environments
                        }
                        _ => panic!("unexpected error: {:?}", e),
                    }
                }
            }
        }

        #[test]
        fn test_dxgi_capture_frame() {
            let mut capture = match DxgiCapture::new() {
                Ok(c) => c,
                Err(_) => {
                    return; // Skip if DXGI is not available
                }
            };
            let frame = capture.capture_frame().expect("capture_frame");
            assert!(frame.width > 0);
            assert!(frame.height > 0);
            assert_eq!(
                frame.data.len(),
                (frame.width * frame.height * 4) as usize
            );
            assert!(frame.timestamp_us > 0);
        }
    }
}

// ─── Non-Windows stub ──────────────────────────────────────────────────────

#[cfg(not(windows))]
mod platform {
    use lanremotecontrol_common::capture::{CapturedFrame, CaptureError, ScreenCapture};

    /// Stub returned on non-Windows platforms.
    pub struct DxgiCapture;

    impl DxgiCapture {
        pub fn new() -> Result<Self, CaptureError> {
            Err(CaptureError::UnsupportedOs)
        }

        pub fn capture_frame(&mut self) -> Result<CapturedFrame, CaptureError> {
            Err(CaptureError::UnsupportedOs)
        }

        pub fn display_info(&self) -> String {
            "DXGI capture: unsupported on this platform".into()
        }
    }

    impl ScreenCapture for DxgiCapture {
        fn capture_frame(&mut self) -> Result<CapturedFrame, CaptureError> {
            self.capture_frame()
        }
    }
}

// ─── Re-export ─────────────────────────────────────────────────────────────

/// Windows DXGI screen capture implementation.
///
/// On non-Windows platforms, [`DxgiCapture::new`] returns
/// [`CaptureError::UnsupportedOs`].
pub use platform::DxgiCapture;
