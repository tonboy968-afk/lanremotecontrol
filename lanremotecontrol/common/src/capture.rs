//! Platform-agnostic screen capture trait.
//!
//! Defines the `ScreenCapture` trait that platform-specific implementations
//! (e.g., DXGI on Windows) must implement, along with shared types.

use std::time::{SystemTime, UNIX_EPOCH};

/// A captured screen frame containing raw BGRA pixel data.
#[derive(Debug, Clone)]
pub struct CapturedFrame {
    /// Raw BGRA pixel data (4 bytes per pixel).
    pub data: Vec<u8>,
    /// Width of the captured frame in pixels.
    pub width: u32,
    /// Height of the captured frame in pixels.
    pub height: u32,
    /// Number of bytes per row (may include stride/padding).
    pub stride: u32,
    /// Monotonic timestamp in microseconds (best-effort).
    pub timestamp_us: u64,
}

impl CapturedFrame {
    /// Create a new `CapturedFrame` with an automatic timestamp.
    pub fn new(data: Vec<u8>, width: u32, height: u32, stride: u32) -> Self {
        let timestamp_us = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_micros() as u64;
        Self {
            data,
            width,
            height,
            stride,
            timestamp_us,
        }
    }

    /// Return the number of pixels in the frame.
    pub fn pixel_count(&self) -> usize {
        (self.width * self.height) as usize
    }
}

/// Errors that can occur during screen capture.
#[derive(Debug)]
pub enum CaptureError {
    /// Initialization of the capture backend failed (with details).
    InitFailed(String),
    /// No display output available.
    NoOutput,
    /// Acquiring a frame from the GPU failed.
    FrameAcquireFailed(String),
    /// The D3D/GPU device was lost and must be re-created.
    DeviceLost,
    /// The current operating system is not supported.
    UnsupportedOs,
}

impl std::fmt::Display for CaptureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InitFailed(msg) => write!(f, "capture init failed: {}", msg),
            Self::NoOutput => write!(f, "no display output available"),
            Self::FrameAcquireFailed(msg) => write!(f, "frame acquire failed: {}", msg),
            Self::DeviceLost => write!(f, "D3D device lost"),
            Self::UnsupportedOs => write!(f, "unsupported operating system"),
        }
    }
}

impl std::error::Error for CaptureError {}

/// Platform-agnostic screen capture trait.
///
/// Each supported OS provides its own implementation (e.g., `DxgiCapture` on
/// Windows, `ScreenCaptureKitCapture` on macOS, etc.).
pub trait ScreenCapture {
    /// Capture the current desktop frame.
    ///
    /// Returns raw BGRA pixel data along with width, height and stride.
    fn capture_frame(&mut self) -> Result<CapturedFrame, CaptureError>;
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_captured_frame_creation() {
        let data = vec![0u8; 1920 * 1080 * 4];
        let frame = CapturedFrame::new(data.clone(), 1920, 1080, 1920 * 4);
        assert_eq!(frame.width, 1920);
        assert_eq!(frame.height, 1080);
        assert_eq!(frame.stride, 1920 * 4);
        assert_eq!(frame.data.len(), 1920 * 1080 * 4);
        assert!(frame.timestamp_us > 0);
    }

    #[test]
    fn test_captured_frame_pixel_count() {
        let frame = CapturedFrame::new(vec![0u8; 640 * 480 * 4], 640, 480, 640 * 4);
        assert_eq!(frame.pixel_count(), 640 * 480);
    }

    #[test]
    fn test_capture_error_display() {
        let err = CaptureError::InitFailed("no adapter".into());
        let msg = format!("{}", err);
        assert!(msg.contains("capture init failed"));
    }
}
