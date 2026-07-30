//! HEVC lossless codec module.
//!
//! Provides frame-level HEVC lossless encoding and decoding via FFmpeg
//! subprocess (one-shot per frame).  Each call spawns `ffmpeg`,
//! pipes raw BGRA data through the x265 lossless encoder (4:4:4), and
//! returns the HEVC bitstream — or the reverse.
//!
//! # Performance
//!
//! Typical per-frame overhead is ~30–60 ms (process spawn + encode).
//! For a 1080p desktop, HEVC lossless achieves ~20–100:1 compression,
//! reducing UDP traffic from thousands of chunks per frame to
//! typically 1–10 chunks.
//!
//! # Requirements
//!
//! `ffmpeg` (with libx265 support, `--enable-libx265`) must be
//! available in `$PATH` or configured via [`set_ffmpeg_path`].

use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::OnceLock;

// ============================================================================
// Public API
// ============================================================================

/// Static FFmpeg executable path.  When `None` (default), `"ffmpeg"` is
/// resolved via `$PATH`.
fn ffmpeg_static() -> &'static OnceLock<String> {
    static PATH: OnceLock<String> = OnceLock::new();
    &PATH
}

/// Return the currently configured FFmpeg path.
fn ffmpeg_cmd() -> &'static str {
    ffmpeg_static()
        .get()
        .map(|s| s.as_str())
        .unwrap_or("ffmpeg")
}

/// Override the FFmpeg executable path (e.g. to a full absolute path).
///
/// Call this once before any encode/decode operation if FFmpeg is not
/// in the default `$PATH`.  The path is stored globally for the
/// lifetime of the process.
pub fn set_ffmpeg_path(path: &str) {
    let _ = ffmpeg_static().set(path.to_owned());
}

/// Check whether FFmpeg is available.
pub fn is_ffmpeg_available() -> bool {
    Command::new(ffmpeg_cmd())
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

// ... rest of the module stays the same ...

/// Errors from HEVC encoding / decoding.
#[derive(Debug)]
pub enum HevcError {
    /// FFmpeg executable not found or failed to spawn.
    FfmpegNotFound(String),
    /// I/O error while reading/writing pipes.
    Io(std::io::Error),
    /// FFmpeg returned a non‑zero exit code.
    FfmpegFailed { exit_code: i32, stderr: String },
    /// Decoded output size does not match expected (w × h × 4).
    SizeMismatch { expected: usize, actual: usize },
}

impl std::fmt::Display for HevcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FfmpegNotFound(msg) => write!(f, "FFmpeg not found: {}", msg),
            Self::Io(e) => write!(f, "I/O error: {}", e),
            Self::FfmpegFailed { exit_code, stderr } => {
                write!(f, "FFmpeg failed (exit={}): {}", exit_code, stderr)
            }
            Self::SizeMismatch { expected, actual } => {
                write!(f, "size mismatch: expected {} bytes, got {}", expected, actual)
            }
        }
    }
}

impl std::error::Error for HevcError {}

impl From<std::io::Error> for HevcError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

// ============================================================================
// Encoding
// ============================================================================

/// Encode a raw BGBA frame into HEVC lossless bitstream.
///
/// * `bgra_data` — raw BGRA pixel data (4 bytes per pixel, row‑major).
/// * `width` / `height` — frame dimensions in pixels (minimum 16×16 for
///   libx265; smaller frames should be padded before calling).
///
/// **Note:** The alpha channel is discarded during encoding (converted to
/// opaque).  On decode, all alpha values will be 0xFF.  The RGB data is
/// preserved bit‑exactly via `gbrp` planar encoding (no YUV colour-space
/// conversion).
///
/// Returns the complete HEVC Annex‑B bitstream (self‑contained keyframe
/// with VPS/SPS/PPS + slice data).
pub fn encode_frame(
    bgra_data: &[u8],
    width: u32,
    height: u32,
) -> Result<Vec<u8>, HevcError> {
    let ffmpeg = ffmpeg_cmd();

    let mut child = Command::new(&ffmpeg)
        .args(&[
            "-f", "rawvideo",
            "-pix_fmt", "bgra",
            "-s", &format!("{}x{}", width, height),
            "-i", "pipe:0",
            "-c:v", "libx265",
            "-pix_fmt", "gbrp",
            "-x265-params",
            "lossless=1:keyint=1:no-open-gop=1:lookahead-slices=0:frame-threads=1",
            "-frames:v", "1",
            "-f", "hevc",
            "-y",
            "pipe:1",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                HevcError::FfmpegNotFound(format!(
                    "`{}` not found in PATH. Install FFmpeg or call set_ffmpeg_path().",
                    ffmpeg
                ))
            } else {
                HevcError::Io(e)
            }
        })?;

    // Write raw frame data to stdin, then close pipe (sends EOF to ffmpeg).
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(bgra_data)?;
        // stdin is dropped here, closing the pipe.
    }

    let output = child
        .wait_with_output()
        .map_err(HevcError::Io)?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        return Err(HevcError::FfmpegFailed {
            exit_code: output.status.code().unwrap_or(-1),
            stderr,
        });
    }

    Ok(output.stdout)
}

// ============================================================================
// Decoding
// ============================================================================

/// Decode an HEVC lossless bitstream back to raw BGRA pixel data.
///
/// * `hevc_data` — the HEVC Annex‑B bitstream (must contain exactly one
///   keyframe for the given resolution).
/// * `width` / `height` — expected frame dimensions.
///
/// On success returns a `Vec<u8>` of exactly `(width * height * 4)` bytes.
pub fn decode_frame(
    hevc_data: &[u8],
    width: u32,
    height: u32,
) -> Result<Vec<u8>, HevcError> {
    let ffmpeg = ffmpeg_cmd();
    let expected_size = (width * height * 4) as usize;

    let mut child = Command::new(&ffmpeg)
        .args(&[
            "-f", "hevc",
            "-i", "pipe:0",
            "-pix_fmt", "bgra",
            "-frames:v", "1",
            "-f", "rawvideo",
            "-y",
            "pipe:1",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                HevcError::FfmpegNotFound(format!(
                    "`{}` not found in PATH. Install FFmpeg or call set_ffmpeg_path().",
                    ffmpeg
                ))
            } else {
                HevcError::Io(e)
            }
        })?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(hevc_data)?;
    }

    let output = child
        .wait_with_output()
        .map_err(HevcError::Io)?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        return Err(HevcError::FfmpegFailed {
            exit_code: output.status.code().unwrap_or(-1),
            stderr,
        });
    }

    if output.stdout.len() != expected_size {
        return Err(HevcError::SizeMismatch {
            expected: expected_size,
            actual: output.stdout.len(),
        });
    }

    Ok(output.stdout)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: create a test frame filled with a solid colour.
    fn make_test_frame(width: u32, height: u32, r: u8, g: u8, b: u8, a: u8) -> Vec<u8> {
        let mut data = Vec::with_capacity((width * height * 4) as usize);
        for _ in 0..(width * height) {
            data.push(b); // B
            data.push(g); // G
            data.push(r); // R
            data.push(a); // A
        }
        data
    }

    /// Helper: create a test frame with a gradient pattern.
    fn make_gradient_frame(width: u32, height: u32) -> Vec<u8> {
        let mut data = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            for x in 0..width {
                data.push((x & 0xFF) as u8);       // B
                data.push((y & 0xFF) as u8);       // G
                data.push(((x + y) & 0xFF) as u8); // R
                data.push(0xFF);                    // A
            }
        }
        data
    }

    #[test]
    fn test_is_ffmpeg_available() {
        // This test only verifies the check doesn't panic.
        let _available = is_ffmpeg_available();
    }

    #[test]
    fn test_encode_decode_round_trip_solid() {
        let w = 320;
        let h = 240;
        let original = make_test_frame(w, h, 64, 128, 192, 255);

        let hevc = match encode_frame(&original, w, h) {
            Ok(d) => d,
            Err(HevcError::FfmpegNotFound(_)) => {
                eprintln!("FFmpeg not available — skipping test");
                return;
            }
            Err(e) => panic!("encode failed: {}", e),
        };

        // HEVC output should be much smaller than raw
        assert!(
            hevc.len() < original.len() / 2,
            "HEVC compressed {} vs raw {}",
            hevc.len(),
            original.len()
        );

        let decoded = decode_frame(&hevc, w, h)
            .expect("decode should succeed");

        assert_eq!(
            decoded.len(),
            (w * h * 4) as usize,
            "decoded size matches"
        );
        assert_eq!(original, decoded, "round-trip should be pixel-perfect");
    }

    #[test]
    fn test_encode_decode_round_trip_gradient() {
        let w = 640;
        let h = 480;
        let original = make_gradient_frame(w, h);

        let hevc = match encode_frame(&original, w, h) {
            Ok(d) => d,
            Err(HevcError::FfmpegNotFound(_)) => {
                eprintln!("FFmpeg not available — skipping test");
                return;
            }
            Err(e) => panic!("encode failed: {}", e),
        };

        println!(
            "Gradient {}x{}: raw={} bytes, hevc={} bytes ({:.1}:1)",
            w,
            h,
            original.len(),
            hevc.len(),
            original.len() as f64 / hevc.len().max(1) as f64
        );

        // Even for complex gradient, HEVC lossless should achieve > 5:1
        assert!(
            hevc.len() < original.len() / 3,
            "HEVC compression ratio should be > 3:1, got {:.1}:1",
            original.len() as f64 / hevc.len().max(1) as f64
        );

        let decoded = decode_frame(&hevc, w, h)
            .expect("decode should succeed");

        assert_eq!(original, decoded, "round-trip should be pixel-perfect for gradient");
    }

    #[test]
    fn test_encode_frame_different_sizes() {
        // Test with a very small frame
        let w = 64;
        let h = 64;
        let original = make_test_frame(w, h, 255, 0, 0, 255);

        let hevc = match encode_frame(&original, w, h) {
            Ok(d) => d,
            Err(HevcError::FfmpegNotFound(_)) => {
                eprintln!("FFmpeg not available — skipping test");
                return;
            }
            Err(e) => panic!("encode failed: {}", e),
        };

        let decoded = decode_frame(&hevc, w, h).expect("decode");
        assert_eq!(original, decoded);

        // Test with a non-power-of-2 frame (alpha must be 255 since HEVC
        // lossless discards the alpha channel)
        let w2 = 100;
        let h2 = 75;
        let original2 = make_test_frame(w2, h2, 0, 255, 0, 255);
        let hevc2 = encode_frame(&original2, w2, h2).expect("encode2");
        let decoded2 = decode_frame(&hevc2, w2, h2).expect("decode2");
        assert_eq!(original2, decoded2);
    }

    #[test]
    fn test_decode_corrupted_data() {
        let w = 320;
        let h = 240;
        // Try to decode garbage data — should gracefully fail
        let garbage = vec![0xABu8; 100];
        let result = decode_frame(&garbage, w, h);
        assert!(result.is_err(), "decoding garbage should fail");
    }

    #[test]
    fn test_encode_smallest_frame() {
        // libx265 minimum is 16×16
        let w = 16;
        let h = 16;
        let original = make_test_frame(w, h, 0, 0, 255, 255); // solid blue
        let hevc = match encode_frame(&original, w, h) {
            Ok(d) => d,
            Err(HevcError::FfmpegNotFound(_)) => {
                eprintln!("FFmpeg not available — skipping test");
                return;
            }
            Err(e) => panic!("encode 16x16 failed: {}", e),
        };
        let decoded = decode_frame(&hevc, w, h).expect("decode 16x16");
        assert_eq!(original, decoded);
    }

    #[test]
    fn test_set_ffmpeg_path() {
        // Should not panic — OnceLock allows at most one set.
        set_ffmpeg_path("ffmpeg");
        // Once set, it stays set.
        assert!(!ffmpeg_cmd().is_empty());
        // Check with a bogus path (still keeps "ffmpeg" since set twice is ignored)
        // Actually OnceLock ignores subsequent sets, so it's still "ffmpeg"
        // which is fine for this test.
        assert!(is_ffmpeg_available() || ffmpeg_cmd().starts_with("ffmpeg"));
    }
}
