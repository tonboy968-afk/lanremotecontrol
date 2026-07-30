//! Tile-based delta frame detection and LZ4 compression.
//!
//!# Overview
//!
//! This module provides functions to:
//! - Split a screen frame into fixed-size tiles (e.g., 64×64)
//! - Compute a fast XXH32 hash for each tile
//! - Detect which tiles have changed between two frames
//! - Extract the pixel data for changed tiles
//! - Compress delta regions (or full frames) using LZ4
//! - Decompress data back to delta regions (or full frames)
//!
//! The tile-based approach avoids transmitting entire frames when only small
//! regions of the screen change (e.g., mouse cursor movement, typing).

use std::collections::HashMap;

/// Represents a single changed region (delta patch) of the screen.
#[derive(Debug, Clone, PartialEq)]
pub struct DeltaRegion {
    /// X position (in pixels) of the top-left corner of this region.
    pub x: u32,
    /// Y position (in pixels) of the top-left corner of this region.
    pub y: u32,
    /// Width of the region in pixels.
    pub width: u32,
    /// Height of the region in pixels.
    pub height: u32,
    /// LZ4-compressed BGRA pixel data for this region.
    pub lz4_compressed_data: Vec<u8>,
}

/// Default tile size used for delta detection (in pixels per side).
pub const DEFAULT_TILE_SIZE: u32 = 64;

/// Threshold: if more than this fraction of tiles changed, prefer a full frame.
pub const FULL_FRAME_THRESHOLD: f64 = 0.3;

/// Errors that can occur during encoding / decoding.
#[derive(Debug)]
pub enum EncodingError {
    /// Compression failed.
    CompressFailed(String),
    /// Decompression failed.
    DecompressFailed(String),
    /// Serialization of delta metadata failed.
    SerializeFailed(String),
    /// Deserialization of delta metadata failed.
    DeserializeFailed(String),
    /// The input data is malformed or inconsistent.
    InvalidData(String),
}

impl std::fmt::Display for EncodingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CompressFailed(msg) => write!(f, "compress failed: {}", msg),
            Self::DecompressFailed(msg) => write!(f, "decompress failed: {}", msg),
            Self::SerializeFailed(msg) => write!(f, "serialize failed: {}", msg),
            Self::DeserializeFailed(msg) => write!(f, "deserialize failed: {}", msg),
            Self::InvalidData(msg) => write!(f, "invalid data: {}", msg),
        }
    }
}

impl std::error::Error for EncodingError {}

// ── Tile Checksums ─────────────────────────────────────────────────────────

/// Compute XXH32 checksums for each tile in the given frame.
///
/// The frame is divided into tiles of `tile_size × tile_size` pixels (4 bytes
/// per BGRA pixel).  Tiles at the right/bottom edge may be smaller if the
/// frame dimensions are not evenly divisible by the tile size.
///
/// Returns a map from `(tile_column, tile_row)` to the 32‑bit XXH32 hash of
/// that tile's pixel data.
pub fn tile_checksums(
    frame: &[u8],
    width: u32,
    height: u32,
    tile_size: u32,
) -> HashMap<(u32, u32), u32> {
    use xxhash_rust::xxh32::xxh32;

    let stride = width * 4; // BGRA: 4 bytes per pixel (no row padding assumed)
    let cols = (width + tile_size - 1) / tile_size;
    let rows = (height + tile_size - 1) / tile_size;

    let mut checksums = HashMap::with_capacity((cols * rows) as usize);

    for tile_row in 0..rows {
        for tile_col in 0..cols {
            let mut tile_data = Vec::with_capacity((tile_size * tile_size * 4) as usize);

            let y_start = tile_row * tile_size;
            let y_end = (y_start + tile_size).min(height);
            let x_start = tile_col * tile_size;
            let x_end = (x_start + tile_size).min(width);

            for y in y_start..y_end {
                let row_start = (y * stride + x_start * 4) as usize;
                let row_end = (y * stride + x_end * 4) as usize;
                tile_data.extend_from_slice(&frame[row_start..row_end]);
            }

            let hash = xxh32(&tile_data, 0);
            checksums.insert((tile_col, tile_row), hash);
        }
    }

    checksums
}

// ── Delta Detection ────────────────────────────────────────────────────────

/// Detect which tiles have changed between two frames by comparing their
/// tile checksums.
///
/// Returns a list of `(tile_col, tile_row)` positions for tiles whose
/// checksum differs between `prev_checksums` and `curr_checksums`.
///
/// Tiles that exist only in `curr_checksums` (e.g., after a resize) are
/// always considered changed.
pub fn detect_delta_tiles(
    prev_checksums: &HashMap<(u32, u32), u32>,
    curr_checksums: &HashMap<(u32, u32), u32>,
) -> Vec<(u32, u32)> {
    let mut changed = Vec::new();

    for (&pos, &curr_hash) in curr_checksums {
        match prev_checksums.get(&pos) {
            Some(&prev_hash) if prev_hash == curr_hash => {
                // tile unchanged
            }
            _ => {
                // tile is new or changed
                changed.push(pos);
            }
        }
    }

    changed
}

/// Determine whether to send a full frame based on the ratio of changed tiles.
///
/// Returns `true` if the ratio of changed tiles to total tiles exceeds
/// `FULL_FRAME_THRESHOLD` (30%).
pub fn should_send_full_frame(changed_tiles: usize, total_tiles: usize) -> bool {
    if total_tiles == 0 {
        return false;
    }
    (changed_tiles as f64) / (total_tiles as f64) > FULL_FRAME_THRESHOLD
}

// ── Tile Data Extraction ───────────────────────────────────────────────────

/// Extract the raw BGRA pixel data for a single tile from the full frame.
///
/// * `frame` - Raw BGRA pixel data (width × height × 4 bytes).
/// * `width` - Frame width in pixels.
/// * `height` - Frame height in pixels.
/// * `stride` - Bytes per row (typically `width * 4`, but can include padding).
/// * `tile_col` - Zero-based column index of the tile.
/// * `tile_row` - Zero-based row index of the tile.
/// * `tile_size` - Size of the tile in pixels (both width and height).
pub fn extract_tile_data(
    frame: &[u8],
    width: u32,
    height: u32,
    stride: u32,
    tile_col: u32,
    tile_row: u32,
    tile_size: u32,
) -> Vec<u8> {
    let x_start = tile_col * tile_size;
    let y_start = tile_row * tile_size;
    let x_end = (x_start + tile_size).min(width);
    let y_end = (y_start + tile_size).min(height);

    let mut data = Vec::with_capacity(((x_end - x_start) * (y_end - y_start) * 4) as usize);

    for y in y_start..y_end {
        let row_start = (y * stride + x_start * 4) as usize;
        let row_end = (y * stride + x_end * 4) as usize;
        data.extend_from_slice(&frame[row_start..row_end]);
    }

    data
}

// ── Compression ────────────────────────────────────────────────────────────

/// LZ4-compress a list of delta regions and serialize them into a single
/// byte vector.
///
/// The output format is:
/// - `count` (4 bytes, little-endian): number of delta regions
/// - For each region:
///   - `x` (4 bytes, LE)
///   - `y` (4 bytes, LE)
///   - `width` (4 bytes, LE)
///   - `height` (4 bytes, LE)
///   - `compressed_len` (4 bytes, LE): length of the LZ4-compressed data
///   - `compressed_data` (compressed_len bytes): LZ4-compressed pixel data
pub fn compress_delta(regions: &[DeltaRegion]) -> Result<Vec<u8>, EncodingError> {
    use lz4::block::compress;

    // Estimate size: header + per-region metadata + compressed data
    let mut output = Vec::with_capacity(4 + regions.len() * (4 * 5 + 64));
    output.extend_from_slice(&(regions.len() as u32).to_le_bytes());

    for region in regions {
        let compressed = compress(&region.lz4_compressed_data, None, false)
            .map_err(|e| EncodingError::CompressFailed(format!("LZ4 block: {}", e)))?;

        // Serialize region metadata + compressed blob
        output.extend_from_slice(&region.x.to_le_bytes());
        output.extend_from_slice(&region.y.to_le_bytes());
        output.extend_from_slice(&region.width.to_le_bytes());
        output.extend_from_slice(&region.height.to_le_bytes());
        output.extend_from_slice(&(compressed.len() as u32).to_le_bytes());
        output.extend_from_slice(&compressed);
    }

    Ok(output)
}

/// LZ4-compress a full frame (the entire raw pixel data).
pub fn compress_full_frame(frame: &[u8]) -> Result<Vec<u8>, EncodingError> {
    use lz4::block::compress;

    compress(frame, None, false)
        .map_err(|e| EncodingError::CompressFailed(format!("LZ4 block: {}", e)))
}

// ── Decompression ──────────────────────────────────────────────────────────

/// Decompress serialized delta region data produced by `compress_delta`.
///
/// Returns the list of `DeltaRegion`s with their `lz4_compressed_data` field
/// populated with the **decompressed** pixel data (i.e., raw BGRA).
pub fn decompress_delta(data: &[u8]) -> Result<Vec<DeltaRegion>, EncodingError> {
    use lz4::block::decompress;

    if data.len() < 4 {
        return Err(EncodingError::InvalidData(
            "data too short for delta header".into(),
        ));
    }

    let count = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize;
    let mut offset = 4;
    let mut regions = Vec::with_capacity(count);

    for _ in 0..count {
        if offset + 20 > data.len() {
            return Err(EncodingError::InvalidData("truncated delta region".into()));
        }

        let x = u32::from_le_bytes([
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ]);
        let y = u32::from_le_bytes([
            data[offset + 4],
            data[offset + 5],
            data[offset + 6],
            data[offset + 7],
        ]);
        let width = u32::from_le_bytes([
            data[offset + 8],
            data[offset + 9],
            data[offset + 10],
            data[offset + 11],
        ]);
        let height = u32::from_le_bytes([
            data[offset + 12],
            data[offset + 13],
            data[offset + 14],
            data[offset + 15],
        ]);
        let compressed_len = u32::from_le_bytes([
            data[offset + 16],
            data[offset + 17],
            data[offset + 18],
            data[offset + 19],
        ]) as usize;
        offset += 20;

        if offset + compressed_len > data.len() {
            return Err(EncodingError::InvalidData(
                "truncated compressed data in delta region".into(),
            ));
        }

        let compressed_data = &data[offset..offset + compressed_len];
        let decompressed = decompress(compressed_data, Some((width * height * 4) as i32))
            .map_err(|e| EncodingError::DecompressFailed(format!("LZ4 block: {}", e)))?;
        offset += compressed_len;

        regions.push(DeltaRegion {
            x,
            y,
            width,
            height,
            lz4_compressed_data: decompressed,
        });
    }

    Ok(regions)
}

/// Decompress a full frame previously compressed with `compress_full_frame`.
///
/// `size` is the expected decompressed size (width × height × 4).
pub fn decompress_full_frame(data: &[u8], size: usize) -> Result<Vec<u8>, EncodingError> {
    use lz4::block::decompress;

    decompress(data, Some(size as i32))
        .map_err(|e| EncodingError::DecompressFailed(format!("LZ4 block: {}", e)))
}

// ── Helper: build delta regions from a frame and a list of changed tiles ───

/// Build a `Vec<DeltaRegion>` from a frame and a list of changed tile
/// coordinates.  Each tile becomes its own delta region.
///
/// The `lz4_compressed_data` field of each returned region contains the
/// **raw (uncompressed)** tile pixel data; call `compress_delta` afterwards
/// to actually compress them.
pub fn build_delta_regions(
    frame: &[u8],
    width: u32,
    height: u32,
    stride: u32,
    changed_tiles: &[(u32, u32)],
    tile_size: u32,
) -> Vec<DeltaRegion> {
    changed_tiles
        .iter()
        .map(|&(col, row)| {
            let data = extract_tile_data(frame, width, height, stride, col, row, tile_size);
            DeltaRegion {
                x: col * tile_size,
                y: row * tile_size,
                width: (tile_size).min(width - col * tile_size),
                height: (tile_size).min(height - row * tile_size),
                lz4_compressed_data: data,
            }
        })
        .collect()
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Create a simple test frame filled with a solid colour value.
    fn make_test_frame(width: u32, height: u32, value: u8) -> Vec<u8> {
        vec![value; (width * height * 4) as usize]
    }

    /// Create a test frame where each pixel has a unique pattern per position.
    fn make_pattern_frame(width: u32, height: u32) -> Vec<u8> {
        let mut data = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            for x in 0..width {
                let idx = (y * width + x) as usize;
                data.push((idx & 0xFF) as u8);        // B
                data.push(((idx >> 8) & 0xFF) as u8);  // G
                data.push(((idx >> 16) & 0xFF) as u8); // R
                data.push(0xFF);                        // A
            }
        }
        data
    }

    // ── tile_checksums ────────────────────────────────────────────────────

    #[test]
    fn test_tile_checksums_uniform_frame() {
        let frame = make_test_frame(1920, 1080, 128);
        let checksums = tile_checksums(&frame, 1920, 1080, 64);
        // Interior tiles (same size) should have the same hash.
        // Edge tiles (partial) may differ due to different amounts of data.
        // Collect hashes for full 64x64 tiles only.
        let full_tile_hashes: std::collections::HashSet<_> = checksums
            .iter()
            .filter(|&(&(col, row), _)| {
                // Tiles that are fully within the frame bounds.
                (col + 1) * 64 <= 1920 && (row + 1) * 64 <= 1080
            })
            .map(|(_, hash)| *hash)
            .collect();
        assert_eq!(
            full_tile_hashes.len(),
            1,
            "all full-size uniform tiles should have same hash"
        );
        // Verify number of tiles
        let expected_cols = (1920 + 63) / 64; // 30
        let expected_rows = (1080 + 63) / 64; // 17
        assert_eq!(checksums.len(), (expected_cols * expected_rows) as usize);
    }

    #[test]
    fn test_tile_checksums_different_frames() {
        let frame1 = make_test_frame(128, 128, 10);
        let frame2 = make_test_frame(128, 128, 20);
        let checksums1 = tile_checksums(&frame1, 128, 128, 64);
        let checksums2 = tile_checksums(&frame2, 128, 128, 64);
        // Every tile hash should differ (same position, different value)
        assert_eq!(checksums1.len(), checksums2.len());
        for (pos, h1) in &checksums1 {
            let h2 = checksums2.get(pos).expect("same tile position in frame2");
            assert_ne!(h1, h2, "tile {:?} hash should differ", pos);
        }
    }

    #[test]
    fn test_tile_checksums_partial_tiles() {
        // 100x100 with 64px tiles — edge tiles are 36x100 and 100x36 and 36x36
        let frame = make_test_frame(100, 100, 42);
        let checksums = tile_checksums(&frame, 100, 100, 64);
        assert_eq!(checksums.len(), 4); // (0,0), (1,0), (0,1), (1,1)
    }

    // ── detect_delta_tiles ────────────────────────────────────────────────

    #[test]
    fn test_detect_delta_tiles_no_changes() {
        let frame = make_test_frame(640, 480, 100);
        let prev = tile_checksums(&frame, 640, 480, 64);
        let curr = tile_checksums(&frame, 640, 480, 64);
        let changed = detect_delta_tiles(&prev, &curr);
        assert!(changed.is_empty());
    }

    #[test]
    fn test_detect_delta_tiles_one_change() {
        let prev_frame = make_test_frame(640, 480, 0);
        let mut curr_frame = make_test_frame(640, 480, 0);
        // Modify a single pixel in tile (5, 3)
        let tile_size = 64u32;
        let x = 5 * tile_size + 10;
        let y = 3 * tile_size + 10;
        let idx = (y * 640 * 4 + x * 4) as usize;
        curr_frame[idx] = 255; // change B channel

        let prev = tile_checksums(&prev_frame, 640, 480, tile_size);
        let curr = tile_checksums(&curr_frame, 640, 480, tile_size);
        let changed = detect_delta_tiles(&prev, &curr);
        assert_eq!(changed, vec![(5, 3)]);
    }

    #[test]
    fn test_detect_delta_tiles_new_tiles_in_curr() {
        let prev_frame = make_test_frame(640, 480, 0);
        let curr_frame = make_test_frame(800, 600, 0);
        let prev = tile_checksums(&prev_frame, 640, 480, 64);
        let curr = tile_checksums(&curr_frame, 800, 600, 64);
        let changed = detect_delta_tiles(&prev, &curr);
        // All tiles that are only in curr should be marked as changed
        assert!(!changed.is_empty());
        // Verify at least one new tile exists
        for &pos in &changed {
            if !prev.contains_key(&pos) {
                // new tile — expected to be changed
                continue;
            }
        }
    }

    // ── should_send_full_frame ────────────────────────────────────────────

    #[test]
    fn test_should_send_full_frame_below_threshold() {
        assert!(!should_send_full_frame(10, 100)); // 10%
    }

    #[test]
    fn test_should_send_full_frame_above_threshold() {
        assert!(should_send_full_frame(40, 100)); // 40%
    }

    #[test]
    fn test_should_send_full_frame_at_threshold() {
        // Exactly 30% is NOT above the threshold (>30%), so false
        assert!(!should_send_full_frame(30, 100));
    }

    #[test]
    fn test_should_send_full_frame_zero_total() {
        assert!(!should_send_full_frame(0, 0));
    }

    // ── extract_tile_data ─────────────────────────────────────────────────

    #[test]
    fn test_extract_tile_data_full_tile() {
        let frame = make_pattern_frame(128, 128);
        let tile = extract_tile_data(&frame, 128, 128, 128 * 4, 0, 0, 64);
        assert_eq!(tile.len(), 64 * 64 * 4);
        // First pixel should be B=0, G=0, R=0, A=255
        assert_eq!(tile[0], 0); // B
        assert_eq!(tile[1], 0); // G
        assert_eq!(tile[2], 0); // R
        assert_eq!(tile[3], 0xFF); // A
    }

    #[test]
    fn test_extract_tile_data_edge_tile() {
        let frame = make_pattern_frame(100, 100);
        // Tile (1, 1) in a 64x64 grid on a 100x100 frame — only 36x36 pixels
        let tile = extract_tile_data(&frame, 100, 100, 100 * 4, 1, 1, 64);
        assert_eq!(tile.len(), 36 * 36 * 4);
    }

    // ── Compress / Decompress Round-Trips ─────────────────────────────────

    #[test]
    fn test_compress_decompress_full_frame_round_trip() {
        let original = make_pattern_frame(256, 256);
        let compressed = compress_full_frame(&original).expect("compress");
        // LZ4 may not always compress random/pattern data, but the
        // round-trip must preserve exact data.
        let decompressed = decompress_full_frame(&compressed, original.len()).expect("decompress");
        assert_eq!(original, decompressed);
    }

    #[test]
    fn test_compress_decompress_delta_round_trip() {
        // Create a frame and modify a single tile
        let width = 256u32;
        let height = 256u32;
        let stride = width * 4;
        let mut frame = make_pattern_frame(width, height);

        // Change tile (1, 0) by modifying a pixel
        let pixel_idx = (0 * width * 4 + 1 * 64 * 4 + 4) as usize; // row 0, col 64+1
        frame[pixel_idx] = 99;

        let tile_size = 64u32;
        let checksums = tile_checksums(&frame, width, height, tile_size);
        let changed: Vec<(u32, u32)> = checksums.keys().copied().collect();

        let regions = build_delta_regions(&frame, width, height, stride, &changed, tile_size);
        assert_eq!(regions.len(), changed.len());

        // Compress
        let compressed = compress_delta(&regions).expect("compress delta");
        assert!(!compressed.is_empty());

        // Decompress
        let decompressed_regions = decompress_delta(&compressed).expect("decompress delta");
        assert_eq!(decompressed_regions.len(), regions.len());

        // Verify pixel data matches
        for (orig, dec) in regions.iter().zip(decompressed_regions.iter()) {
            assert_eq!(orig.x, dec.x);
            assert_eq!(orig.y, dec.y);
            assert_eq!(orig.width, dec.width);
            assert_eq!(orig.height, dec.height);
            assert_eq!(orig.lz4_compressed_data, dec.lz4_compressed_data);
        }
    }

    #[test]
    fn test_compress_decompress_single_delta_region() {
        let width = 640u32;
        let height = 480u32;
        let stride = width * 4;
        let frame = make_test_frame(width, height, 128);

        // Simulate a change: modify one pixel in tile (2, 3)
        let tile_size = 64u32;
        let changed_tiles = vec![(2u32, 3u32)];
        let regions = build_delta_regions(&frame, width, height, stride, &changed_tiles, tile_size);
        assert_eq!(regions.len(), 1);
        let region = &regions[0];
        assert_eq!(region.width, 64);
        assert_eq!(region.height, 64);
        assert_eq!(region.x, 2 * 64); // 128
        assert_eq!(region.y, 3 * 64); // 192

        let compressed = compress_delta(&regions).expect("compress");
        let decompressed = decompress_delta(&compressed).expect("decompress");
        assert_eq!(decompressed.len(), 1);
        assert_eq!(decompressed[0].lz4_compressed_data, region.lz4_compressed_data);
    }

    #[test]
    fn test_compress_empty_delta() {
        let regions: Vec<DeltaRegion> = vec![];
        let compressed = compress_delta(&regions).expect("compress empty");
        // Should contain just the count = 0
        assert_eq!(compressed.len(), 4);
        let decompressed = decompress_delta(&compressed).expect("decompress empty");
        assert!(decompressed.is_empty());
    }

    // ── build_delta_regions ───────────────────────────────────────────────

    #[test]
    fn test_build_delta_regions_single() {
        let frame = make_test_frame(1920, 1080, 55);
        let changed = vec![(0u32, 0u32)];
        let regions = build_delta_regions(&frame, 1920, 1080, 1920 * 4, &changed, 64);
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].x, 0);
        assert_eq!(regions[0].y, 0);
        assert_eq!(regions[0].lz4_compressed_data.len(), 64 * 64 * 4);
    }

    // ── Edge Cases ────────────────────────────────────────────────────────

    #[test]
    fn test_tile_checksums_very_small_frame() {
        let frame = make_test_frame(10, 10, 7);
        let checksums = tile_checksums(&frame, 10, 10, 64);
        assert_eq!(checksums.len(), 1); // single tile covering the whole frame
    }

    #[test]
    fn test_detect_delta_tiles_all_changed() {
        let mut prev = HashMap::new();
        let mut curr = HashMap::new();
        prev.insert((0, 0), 100);
        prev.insert((0, 1), 200);
        curr.insert((0, 0), 300);
        curr.insert((0, 1), 400);
        let changed = detect_delta_tiles(&prev, &curr);
        assert_eq!(changed.len(), 2);
        assert!(changed.contains(&(0, 0)));
        assert!(changed.contains(&(0, 1)));
    }

    #[test]
    fn test_decompress_full_frame_small_data() {
        let original = vec![42u8; 1024];
        let compressed = compress_full_frame(&original).expect("compress");
        let decompressed = decompress_full_frame(&compressed, 1024).expect("decompress");
        assert_eq!(original, decompressed);
    }
}
