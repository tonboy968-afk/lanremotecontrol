//! Windows H.264/H.265 Hardware Encoding - Implementation Guide
//!
//! # Overview
//!
//! This module documents the approach used by commercial remote desktop software 
//! like ToDesk, Sunflower, and RayLink to achieve zero-dependency H.264/H.265 
//! hardware encoding on Windows.
//!
//! ## How Commercial Software Achieves Zero-Dependency H.264/H.265 Encoding
//!
//! Commercial remote desktop software does NOT require users to install FFmpeg or 
//! any additional dependencies. Instead, they use **Windows built-in hardware encoders**:
//!
//! 1. **NVIDIA NVENC** - For NVIDIA GPUs (via `nvencodeapi.h`)
//! 2. **AMD AMF/VCE** - For AMD GPUs (via AMD Video Codec SDK)
//! 3. **Intel Quick Sync Video (QSV)** - For Intel integrated/discrete GPUs
//! 4. **Microsoft H.264/HEVC Video Encoder MFT** - Built into Windows 10/11 Media Foundation
//!
//! ## Implementation Approaches in Rust
//!
//! ### Approach 1: Windows Media Foundation MFT (Recommended for MVP)
//! Use `MFEnumTransforms` with category `MFT_CATEGORY_VIDEO_ENCODER` and subtype 
//! `MFVideoFormat_H264` to enumerate the built-in H.264 encoder, then use 
//! `IMFActivate::CreateInstance` to create the MFT instance.
//!
//! ### Approach 2: WHEE (Windows Hardware Encoding Extension)
//! Use Direct3D11 + Media Foundation Sink Writer with hardware-accelerated encoders.
//! This is what ToDesk/Sunflower primarily use for optimal performance.
//!
//! ### Approach 3: GPU-Specific APIs
//! - NVIDIA: `nvenc` crate or bind to `nvencodeapi.h`
//! - Intel QSV: `qsv-rust` crate or Media Foundation
//! - AMD: AMF SDK bindings
//!
//! ## Why LZ4 Fallback is Used in This MVP
//!
//! The current implementation uses LZ4 compression as a working baseline because:
//! 1. Full MFT pipeline integration requires complex COM/IMFSample handling
//! 2. BGRA to NV12 color space conversion is needed for H.264 encoders
//! 3. Proper D3D11 texture sharing with the encoder MFT requires careful synchronization
//!
//! ## Next Steps for Production H.264 Encoding
//!
//! To implement production-ready H.264 hardware encoding:
//! 1. Add the `windows-media-foundation` crate or use proper `windows` crate features
//! 2. Implement BGRA to NV12 conversion using D3D11 shader or CPU fallback
//! 3. Use `MFCreateSinkWriterToURL` or `IMFTransform` with proper `IMFSample` creation
//! 4. Handle keyframe (IDR) generation periodically for streaming resilience

use std::ptr;
use windows::core::{Interface, GUID};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};

/// H.264 Encoder error type
#[derive(Debug)]
pub enum H264EncoderError {
    /// COM initialization failed
    ComInitFailed(String),
    /// Media Foundation startup failed
    MFStartupFailed(String),
    /// H.264 encoding not fully implemented in MVP (requires MFT pipeline)
    NotImplemented(String),
}

impl std::fmt::Display for H264EncoderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ComInitFailed(msg) => write!(f, "COM init failed: {}", msg),
            Self::MFStartupFailed(msg) => write!(f, "Media Foundation startup failed: {}", msg),
            Self::NotImplemented(msg) => write!(f, "H.264 MFT encoding not fully implemented: {}", msg),
        }
    }
}

impl std::error::Error for H264EncoderError {}

/// NV12 pixel format buffer structure (for reference)
#[derive(Debug, Clone)]
pub struct Nv12Buffer {
    pub y_plane: Vec<u8>,
    pub uv_plane: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// BGR/A to NV12 converter (BT.601 YUV conversion)
pub fn bgra_to_nv12(bgra_data: &[u8], width: u32, height: u32) -> Nv12Buffer {
    let y_plane_size = (width * height) as usize;
    let uv_plane_size = ((width / 2) * (height / 2)) as usize * 2;
    
    let mut y_plane = vec![0u8; y_plane_size];
    let mut uv_plane = vec![0u8; uv_plane_size];

    for y in 0..height {
        for x in 0..width {
            let src_idx = ((y * width + x) * 4) as usize;
            if src_idx + 3 >= bgra_data.len() {
                continue;
            }
            
            let b = bgra_data[src_idx] as f32;
            let g = bgra_data[src_idx + 1] as f32;
            let r = bgra_data[src_idx + 2] as f32;

            // BT.601 YUV conversion
            let y_val = (0.299 * r + 0.587 * g + 0.114 * b + 0.5) as u8;
            let u_val = (-0.14713 * r - 0.28886 * g + 0.436 * b + 128.5 + 0.5) as u8;
            let v_val = (0.615 * r - 0.51499 * g - 0.10001 * b + 128.5 + 0.5) as u8;

            y_plane[(y * width + x) as usize] = y_val;

            // UV sampling at even coordinates
            if y % 2 == 0 && x % 2 == 0 {
                let uv_idx = ((y / 2) * (width / 2) + (x / 2)) as usize * 2;
                uv_plane[uv_idx] = u_val;
                uv_plane[uv_idx + 1] = v_val;
            }
        }
    }

    Nv12Buffer {
        y_plane,
        uv_plane,
        width,
        height,
    }
}

/// H.264 Hardware Encoder instance (MVP stub)
pub struct H264Encoder {
    width: u32,
    height: u32,
}

impl H264Encoder {
    /// Create a new H.264 encoder with the specified dimensions and frame rate
    pub fn new(width: u32, height: u32, _fps: u32) -> Result<Self, H264EncoderError> {
        // Initialize COM
        unsafe {
            let hr = CoInitializeEx(None, COINIT_MULTITHREADED);
            if !hr.is_ok() && hr.as_ptr() as i32 != 0x80010106i32 /* S_FALSE */ {
                return Err(H264EncoderError::ComInitFailed(format!("CoInitializeEx failed: {:?}", hr)));
            }
        }

        // Initialize Media Foundation
        unsafe {
            let hr = MFStartup(MF_VERSION, MFSTARTUP_FULL);
            if hr.is_err() {
                return Err(H264EncoderError::MFStartupFailed(format!("MFStartup failed: {:?}", hr)));
            }
        }

        Ok(Self { width, height })
    }

    /// Encode a raw BGRA frame to H.264 (MVP: returns NotImplemented error)
    pub fn encode_frame(
        &mut self,
        _bgra_data: &[u8],
        _timestamp_us: u64,
    ) -> Result<encoder_types::H264Frame, H264EncoderError> {
        Err(H264EncoderError::NotImplemented(
            "Full MFT pipeline integration required. See module documentation for production implementation guide.".into()
        ))
    }

    /// Flush the encoder (useful when resolution changes or encoding stops)
    pub fn flush(&mut self) -> Result<(), H264EncoderError> {
        Ok(())
    }
}

impl Drop for H264Encoder {
    fn drop(&mut self) {
        // Shutdown Media Foundation
        unsafe {
            let _ = MFShutdown();
        }
        // Uninitialize COM
        unsafe {
            CoUninitialize();
        }
    }
}

/// Release devices array from MFEnumTransforms (utility)
unsafe fn MFReleaseDevices(_devices: *mut IMFActivateArray, _count: u32) {
    // In production, properly release via COM interface
}

/// H.264 encoded frame data types
pub mod encoder_types {
    /// H.264 encoded frame data
    #[derive(Debug, Clone)]
    pub struct H264Frame {
        /// The encoded H.264 NAL units (including start codes)
        pub data: Vec<u8>,
        /// Whether this frame is an IDR frame (keyframe)
        pub is_keyframe: bool,
        /// Presentation timestamp in microseconds
        pub pts_us: u64,
    }
}
