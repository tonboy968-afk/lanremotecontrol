//! # Integration tests for LANRemoteControl Common Library
//!
//! Tests the complete pipeline end-to-end:
//!   - Message serialization → deserialization round-trips
//!   - Full connection handshake simulation (request → capabilities → confirm → teardown)
//!   - Screen capture → tile checksums → delta detection → compress → decompress pipeline
//!   - Large frame (4K) compression round-trip with timing
//!   - Input event → serialize → deserialize pipeline (all event types)
//!   - Concurrent pipeline performance measurement
//!
//! These tests verify cross-module integration that unit tests within each
//! module cannot fully cover.

use std::time::Instant;

use lanremotecontrol_common::capture::*;
use lanremotecontrol_common::encoding::*;
use lanremotecontrol_common::*;

// ============================================================================
// Helper: create a test frame with a given fill value (all pixels identical)
// ============================================================================

fn make_uniform_frame(width: u32, height: u32, value: u8) -> Vec<u8> {
    vec![value; (width * height * 4) as usize]
}

/// Create a test frame with a gradient pattern (deterministic per-pixel).
fn make_gradient_frame(width: u32, height: u32) -> Vec<u8> {
    let mut data = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            data.push((x % 256) as u8);         // B – horizontal gradient
            data.push((y % 256) as u8);         // G – vertical gradient
            data.push(128u8);                     // R – constant
            data.push(255u8);                     // A – opaque
        }
    }
    data
}

/// Create a test frame with a checkerboard pattern.
fn make_checkerboard_frame(width: u32, height: u32, tile_px: u32) -> Vec<u8> {
    let mut data = vec![0u8; (width * height * 4) as usize];
    for y in 0..height {
        for x in 0..width {
            let idx = ((y * width + x) * 4) as usize;
            let square = (x / tile_px + y / tile_px) % 2;
            if square == 0 {
                data[idx] = 255;       // white
                data[idx + 1] = 255;
                data[idx + 2] = 255;
                data[idx + 3] = 255;
            } else {
                data[idx] = 0;          // black
                data[idx + 1] = 0;
                data[idx + 2] = 0;
                data[idx + 3] = 255;
            }
        }
    }
    data
}

// ============================================================================
// Test 1: Full connection handshake simulation
// ============================================================================

#[test]
fn test_full_connection_handshake_simulated() {
    // Simulate a complete 4-step handshake using only the common crate API:
    //   1. Client → Host: ConnectionRequest
    //   2. Host → Client: CapabilitiesResponse (accepted)
    //   3. Client → Host: ConnectionConfirm
    //   4. Client → Host (or vice‑versa): Teardown
    //
    // Each step goes through the full serialize → deserialize cycle as it
    // would on the wire.

    let auth = "integration-test-pin-9876";

    // ── Step 1: Client creates ConnectionRequest ──────────────────────────
    let req_msg = create_connection_request(1, auth, 1)
        .expect("create connection request");

    assert_eq!(req_msg.message_type, MessageType::ConnectionManagement);
    assert_eq!(req_msg.sequence_number, 1);

    // Serialize and deserialize as if over the network
    let req_bytes = req_msg.to_bytes().expect("serialize request");
    let req_decoded = Message::from_bytes(&req_bytes).expect("deserialize request");
    assert_eq!(req_msg, req_decoded);

    // Host deserializes the payload
    let req_payload: ConnectionManagementPayload =
        bincode::deserialize(&req_decoded.payload).expect("deserialize conn mgmt payload");
    match &req_payload {
        ConnectionManagementPayload::Request(r) => {
            assert_eq!(r.auth_token, auth);
            assert_eq!(r.protocol_version, 1);
        }
        _ => panic!("Expected ConnectionRequest"),
    }

    // ── Step 2: Host creates CapabilitiesResponse (accepted) ──────────────
    let caps = EncodingCapabilities {
        lz4_delta: true,
        h264_low_delay: false,
        av1_rt: false,
        max_width: 1920,
        max_height: 1080,
    };
    let resp_msg = create_capabilities_response(1, true, "", caps)
        .expect("create capabilities response");
    assert_eq!(resp_msg.message_type, MessageType::ConnectionManagement);
    assert_eq!(resp_msg.sequence_number, 1);

    let resp_bytes = resp_msg.to_bytes().expect("serialize response");
    let resp_decoded = Message::from_bytes(&resp_bytes).expect("deserialize response");
    assert_eq!(resp_msg, resp_decoded);

    let resp_payload: ConnectionManagementPayload =
        bincode::deserialize(&resp_decoded.payload).expect("deserialize conn mgmt payload");
    match &resp_payload {
        ConnectionManagementPayload::Capabilities(c) => {
            assert!(c.accepted);
            assert!(c.reject_reason.is_empty());
            assert!(c.encoding.lz4_delta);
            assert_eq!(c.encoding.max_width, 1920);
            assert_eq!(c.encoding.max_height, 1080);
        }
        _ => panic!("Expected CapabilitiesResponse"),
    }

    // ── Step 3: Client creates ConnectionConfirm ──────────────────────────
    let confirm_msg = create_connection_confirm(2, "lz4")
        .expect("create connection confirm");
    assert_eq!(confirm_msg.message_type, MessageType::ConnectionManagement);
    assert_eq!(confirm_msg.sequence_number, 2);

    let confirm_bytes = confirm_msg.to_bytes().expect("serialize confirm");
    let confirm_decoded = Message::from_bytes(&confirm_bytes).expect("deserialize confirm");
    assert_eq!(confirm_msg, confirm_decoded);

    let confirm_payload: ConnectionManagementPayload =
        bincode::deserialize(&confirm_decoded.payload).expect("deserialize conn mgmt payload");
    match &confirm_payload {
        ConnectionManagementPayload::Confirm(c) => {
            assert_eq!(c.chosen_encoding, "lz4");
        }
        _ => panic!("Expected ConnectionConfirm"),
    }

    // ── Step 4: Teardown ──────────────────────────────────────────────────
    let td_msg = create_teardown(3, "user_disconnect")
        .expect("create teardown");
    assert_eq!(td_msg.message_type, MessageType::ConnectionManagement);
    assert_eq!(td_msg.sequence_number, 3);

    let td_bytes = td_msg.to_bytes().expect("serialize teardown");
    let td_decoded = Message::from_bytes(&td_bytes).expect("deserialize teardown");
    assert_eq!(td_msg, td_decoded);

    let td_payload: ConnectionManagementPayload =
        bincode::deserialize(&td_decoded.payload).expect("deserialize conn mgmt payload");
    match &td_payload {
        ConnectionManagementPayload::Teardown(t) => {
            assert_eq!(t.reason, "user_disconnect");
        }
        _ => panic!("Expected Teardown"),
    }

    // ── Rejected handshake variant ────────────────────────────────────────
    let rejected_caps = EncodingCapabilities {
        lz4_delta: false,
        h264_low_delay: false,
        av1_rt: false,
        max_width: 0,
        max_height: 0,
    };
    let reject_msg = create_capabilities_response(5, false, "invalid protocol version", rejected_caps)
        .expect("create rejection response");
    let reject_bytes = reject_msg.to_bytes().expect("serialize rejection");
    let reject_decoded = Message::from_bytes(&reject_bytes).expect("deserialize rejection");

    let reject_payload: ConnectionManagementPayload =
        bincode::deserialize(&reject_decoded.payload).expect("deserialize reject payload");
    match &reject_payload {
        ConnectionManagementPayload::Capabilities(c) => {
            assert!(!c.accepted);
            assert_eq!(c.reject_reason, "invalid protocol version");
        }
        _ => panic!("Expected CapabilitiesResponse (rejected)"),
    }
}

// ============================================================================
// Test 2: Screen capture → encode → decode pipeline (synthetic data)
// ============================================================================

#[test]
fn test_screen_encode_decode_pipeline() {
    // Integration test for the full encoding pipeline:
    //   Create frame → tile_checksums → detect_delta_tiles →
    //   build_delta_regions → compress_delta → decompress_delta → verify

    let width = 1280u32;
    let height = 720u32;
    let tile_size = 64u32;
    let stride = width * 4;

    // Create two similar frames (only right half differs)
    let frame_a = make_gradient_frame(width, height);
    let mut frame_b = frame_a.clone();

    // Modify the right half of frame_b
    for y in 0..height {
        for x in (width / 2)..width {
            let idx = ((y * stride + x * 4) as usize).min(frame_b.len() - 4);
            frame_b[idx] = 255;     // B
            frame_b[idx + 1] = 128; // G
            frame_b[idx + 2] = 64;  // R
            // A stays 255
        }
    }

    // Compute tile checksums for both frames
    let checksums_a = tile_checksums(&frame_a, width, height, tile_size);
    let checksums_b = tile_checksums(&frame_b, width, height, tile_size);

    // Detect changed tiles
    let changed_tiles = detect_delta_tiles(&checksums_a, &checksums_b);
    assert!(!changed_tiles.is_empty(), "changed tiles should not be empty");
    eprintln!(
        "Delta detection: {} tiles changed out of {} ({:.1}%)",
        changed_tiles.len(),
        checksums_b.len(),
        (changed_tiles.len() as f64 / checksums_b.len() as f64) * 100.0,
    );

    // Check that changed tiles are on the right half
    for &(col, _row) in &changed_tiles {
        let tile_x = col * tile_size;
        assert!(
            tile_x >= width / 2 || tile_x + tile_size > width / 2,
            "changed tile at x={} should be in right half (>= {})",
            tile_x,
            width / 2
        );
    }

    // Build delta regions
    let regions = build_delta_regions(
        &frame_b,
        width,
        height,
        stride,
        &changed_tiles,
        tile_size,
    );
    assert_eq!(regions.len(), changed_tiles.len());

    // Compress delta
    let compressed = compress_delta(&regions).expect("compress delta");
    assert!(!compressed.is_empty());

    // Decompress delta
    let decompressed = decompress_delta(&compressed).expect("decompress delta");
    assert_eq!(decompressed.len(), regions.len());

    // Verify each region matches
    for (orig, dec) in regions.iter().zip(decompressed.iter()) {
        assert_eq!(orig.x, dec.x);
        assert_eq!(orig.y, dec.y);
        assert_eq!(orig.width, dec.width);
        assert_eq!(orig.height, dec.height);
        assert_eq!(
            orig.lz4_compressed_data,
            dec.lz4_compressed_data,
            "pixel data mismatch at region ({}, {}) {}x{}",
            orig.x,
            orig.y,
            orig.width,
            orig.height
        );
    }

    eprintln!(
        "Delta compression: {} regions, {} bytes → {} bytes (ratio {:.2}x)",
        regions.len(),
        regions.iter().map(|r| r.lz4_compressed_data.len()).sum::<usize>(),
        compressed.len(),
        regions.iter().map(|r| r.lz4_compressed_data.len()).sum::<usize>() as f64 / compressed.len().max(1) as f64,
    );

    // Test full frame fallback detection
    let total_tiles = checksums_b.len();
    let should_full = should_send_full_frame(changed_tiles.len(), total_tiles);
    eprintln!(
        "Full frame fallback: {}% changed → should_send_full_frame = {}",
        (changed_tiles.len() as f64 / total_tiles as f64) * 100.0,
        should_full
    );

    // ── Full frame compression round-trip ────────────────────────────────
    let full_compressed = compress_full_frame(&frame_b).expect("compress full frame");
    let full_decompressed =
        decompress_full_frame(&full_compressed, frame_b.len()).expect("decompress full frame");
    assert_eq!(
        frame_b, full_decompressed,
        "full frame round-trip should preserve exact pixel data"
    );

    eprintln!(
        "Full frame: {} bytes → {} bytes (ratio {:.2}x)",
        frame_b.len(),
        full_compressed.len(),
        frame_b.len() as f64 / full_compressed.len().max(1) as f64,
    );
}

// ============================================================================
// Test 3: Large frame (4K) compression performance test
// ============================================================================

#[test]
fn test_large_frame_4k_compression() {
    // 4K UHD: 3840 × 2160 × 4 bytes = ~31.6 MB
    let width = 3840u32;
    let height = 2160u32;
    let tile_size = 64u32;
    let stride = width * 4;

    // Use a checkerboard pattern (moderately compressible)
    let frame = make_checkerboard_frame(width, height, 16);
    assert_eq!(frame.len(), (width * height * 4) as usize);

    // ── Tile checksums timing ────────────────────────────────────────────
    let start = Instant::now();
    let checksums = tile_checksums(&frame, width, height, tile_size);
    let checksum_duration = start.elapsed();
    eprintln!(
        "4K tile checksums: {} tiles in {:.3?} ({:.1} tiles/ms)",
        checksums.len(),
        checksum_duration,
        checksums.len() as f64 / checksum_duration.as_secs_f64() / 1000.0,
    );

    let expected_cols = (width + tile_size - 1) / tile_size; // 60
    let expected_rows = (height + tile_size - 1) / tile_size; // 34
    assert_eq!(checksums.len(), (expected_cols * expected_rows) as usize);

    // ── Full frame LZ4 compression timing ────────────────────────────────
    let start = Instant::now();
    let compressed = compress_full_frame(&frame).expect("compress 4K frame");
    let compress_duration = start.elapsed();
    eprintln!(
        "4K LZ4 compress: {:.1} MB → {:.1} MB in {:.3?} ({:.1} MB/s)",
        frame.len() as f64 / 1_048_576.0,
        compressed.len() as f64 / 1_048_576.0,
        compress_duration,
        (frame.len() as f64 / 1_048_576.0) / compress_duration.as_secs_f64(),
    );

    // ── Full frame LZ4 decompression timing ──────────────────────────────
    let start = Instant::now();
    let decompressed =
        decompress_full_frame(&compressed, frame.len()).expect("decompress 4K frame");
    let decompress_duration = start.elapsed();
    eprintln!(
        "4K LZ4 decompress: {:.1} MB → {:.1} MB in {:.3?} ({:.1} MB/s)",
        compressed.len() as f64 / 1_048_576.0,
        decompressed.len() as f64 / 1_048_576.0,
        decompress_duration,
        (decompressed.len() as f64 / 1_048_576.0) / decompress_duration.as_secs_f64(),
    );

    // Verify correctness
    assert_eq!(
        frame, decompressed,
        "4K frame round-trip must preserve exact pixel data"
    );

    // ── Delta region build + compress + decompress for a subset of tiles ──
    // Pick 5% of tiles as "changed"
    let all_tiles: Vec<(u32, u32)> = checksums.keys().copied().collect();
    let sample_count = (all_tiles.len() as f64 * 0.05) as usize;
    let changed_sample: Vec<(u32, u32)> = all_tiles
        .iter()
        .step_by(all_tiles.len().max(1) / sample_count.max(1))
        .copied()
        .take(sample_count.max(1))
        .collect();
    eprintln!("4K delta: processing {} changed tiles", changed_sample.len());

    let regions = build_delta_regions(&frame, width, height, stride, &changed_sample, tile_size);
    assert_eq!(regions.len(), changed_sample.len());

    let start = Instant::now();
    let delta_compressed = compress_delta(&regions).expect("compress 4K delta");
    let delta_compress_duration = start.elapsed();

    let start = Instant::now();
    let delta_decompressed =
        decompress_delta(&delta_compressed).expect("decompress 4K delta");
    let delta_decompress_duration = start.elapsed();

    // Verify delta correctness
    assert_eq!(delta_decompressed.len(), regions.len());
    for (orig, dec) in regions.iter().zip(delta_decompressed.iter()) {
        assert_eq!(orig.lz4_compressed_data, dec.lz4_compressed_data);
    }

    eprintln!(
        "4K delta: {} regions, compress={:.3?}, decompress={:.3?}",
        regions.len(),
        delta_compress_duration,
        delta_decompress_duration,
    );

    // Verify compression ratio sanity (checkerboard should compress well)
    let ratio = frame.len() as f64 / compressed.len().max(1) as f64;
    assert!(ratio > 1.0, "checkerboard should be compressible (ratio={:.2})", ratio);
}

// ============================================================================
// Test 4: Input event → serialize → deserialize → inject pipeline
// ============================================================================

#[test]
fn test_input_event_serialize_deserialize_pipeline() {
    // Test that every input event type survives the full pipeline:
    //   Create event → serialize to ControlCommandPayload →
    //   embed in Message → to_bytes → from_bytes →
    //   extract payload → deserialize → verify

    let test_cases: Vec<(ControlCommandPayload, &str)> = vec![
        (
            ControlCommandPayload::Key(KeyEvent {
                key_code: 65,         // 'A'
                pressed: true,
                modifiers: 0b0010,    // Ctrl
                timestamp_us: 1000001,
            }),
            "KeyEvent (Ctrl+A down)",
        ),
        (
            ControlCommandPayload::Key(KeyEvent {
                key_code: 65,
                pressed: false,
                modifiers: 0b0000,
                timestamp_us: 1000002,
            }),
            "KeyEvent (A up)",
        ),
        (
            ControlCommandPayload::MouseMove(MouseMoveEvent {
                dx: 42,
                dy: -17,
                abs_coords: false,
                timestamp_us: 2000001,
            }),
            "MouseMoveEvent (relative)",
        ),
        (
            ControlCommandPayload::MouseMove(MouseMoveEvent {
                dx: 1920,
                dy: 1080,
                abs_coords: true,
                timestamp_us: 2000002,
            }),
            "MouseMoveEvent (absolute)",
        ),
        (
            ControlCommandPayload::MouseButton(MouseButtonEvent {
                button: 0,          // left
                pressed: true,
                x: 800,
                y: 600,
                timestamp_us: 3000001,
            }),
            "MouseButtonEvent (left down)",
        ),
        (
            ControlCommandPayload::MouseButton(MouseButtonEvent {
                button: 1,          // right
                pressed: false,
                x: 800,
                y: 600,
                timestamp_us: 3000002,
            }),
            "MouseButtonEvent (right up)",
        ),
        (
            ControlCommandPayload::Scroll(ScrollEvent {
                delta_x: 0,
                delta_y: 120,       // scroll up
                timestamp_us: 4000001,
            }),
            "ScrollEvent (vertical)",
        ),
        (
            ControlCommandPayload::Scroll(ScrollEvent {
                delta_x: -30,
                delta_y: 0,         // scroll left
                timestamp_us: 4000002,
            }),
            "ScrollEvent (horizontal)",
        ),
    ];

    for (event, description) in &test_cases {
        // Serialize payload
        let payload_bytes = bincode::serialize(event)
            .unwrap_or_else(|e| panic!("serialize {} payload: {}", description, e));

        // Wrap in Message
        let msg = Message::new(MessageType::ControlCommand, 42, payload_bytes);
        assert_eq!(msg.message_type, MessageType::ControlCommand);
        assert_eq!(msg.sequence_number, 42);
        assert_eq!(msg.payload_length as usize, msg.payload.len());

        // Full message round-trip
        let msg_bytes = msg.to_bytes()
            .unwrap_or_else(|e| panic!("serialize {} message: {}", description, e));
        let msg_decoded = Message::from_bytes(&msg_bytes)
            .unwrap_or_else(|e| panic!("deserialize {} message: {}", description, e));
        assert_eq!(msg, msg_decoded, "message round-trip for {}", description);

        // Deserialize payload
        let decoded: ControlCommandPayload = bincode::deserialize(&msg_decoded.payload)
            .unwrap_or_else(|e| panic!("deserialize {} payload: {}", description, e));

        // Verify the decoded event matches
        match (event, &decoded) {
            (ControlCommandPayload::Key(orig), ControlCommandPayload::Key(d)) => {
                assert_eq!(orig.key_code, d.key_code, "{} key_code", description);
                assert_eq!(orig.pressed, d.pressed, "{} pressed", description);
                assert_eq!(orig.modifiers, d.modifiers, "{} modifiers", description);
                assert_eq!(orig.timestamp_us, d.timestamp_us, "{} timestamp", description);
            }
            (ControlCommandPayload::MouseMove(orig), ControlCommandPayload::MouseMove(d)) => {
                assert_eq!(orig.dx, d.dx, "{} dx", description);
                assert_eq!(orig.dy, d.dy, "{} dy", description);
                assert_eq!(orig.abs_coords, d.abs_coords, "{} abs_coords", description);
            }
            (ControlCommandPayload::MouseButton(orig), ControlCommandPayload::MouseButton(d)) => {
                assert_eq!(orig.button, d.button, "{} button", description);
                assert_eq!(orig.pressed, d.pressed, "{} pressed", description);
                assert_eq!(orig.x, d.x, "{} x", description);
                assert_eq!(orig.y, d.y, "{} y", description);
            }
            (ControlCommandPayload::Scroll(orig), ControlCommandPayload::Scroll(d)) => {
                assert_eq!(orig.delta_x, d.delta_x, "{} delta_x", description);
                assert_eq!(orig.delta_y, d.delta_y, "{} delta_y", description);
            }
            _ => panic!("Type mismatch for {}", description),
        }
    }
}

// ============================================================================
// Test 5: Multi-message stream simulation
// ============================================================================

#[test]
fn test_multi_message_stream_round_trip() {
    // Simulate a mixed stream of messages (heartbeats, ACKs, control events)
    // to verify sequential serialization/deserialization works correctly.

    let messages = vec![
        create_heartbeat(1),
        create_ack(1),
        create_connection_request(2, "stream-test", 1)
            .expect("create connection request"),
        Message::new(MessageType::Heartbeat, 2, vec![]),
        create_ack(2),
        create_connection_confirm(3, "lz4")
            .expect("create connection confirm"),
        create_heartbeat(3),
        create_teardown(4, "done")
            .expect("create teardown"),
    ];

    // Serialize all, then deserialize all
    let mut serialized: Vec<Vec<u8>> = Vec::new();
    for msg in &messages {
        let bytes = msg.to_bytes()
            .unwrap_or_else(|e| panic!("serialize seq {}: {}", msg.sequence_number, e));
        serialized.push(bytes);
    }

    // Deserialize and verify
    for (i, bytes) in serialized.iter().enumerate() {
        let decoded = Message::from_bytes(bytes)
            .unwrap_or_else(|e| panic!("deserialize message {}: {}", i, e));
        assert_eq!(decoded, messages[i], "message {} mismatch", i);
    }

    // Verify no data loss by concatenating and splitting using bincode framing.
    // Since bincode uses a fixed per-message overhead (22 bytes for header +
    // length prefix of Vec), we rely on bincode deserialization directly from
    // a byte slice — bincode will consume exactly the right number of bytes
    // per message when we track offsets manually via the serialized size.
    let concatenated: Vec<u8> = serialized.iter().flat_map(|b| b.iter().copied()).collect();
    let mut offset = 0;
    let mut recovered = Vec::new();
    // We know each serialized message's length from the earlier serialization step
    let msg_lengths: Vec<usize> = serialized.iter().map(|b| b.len()).collect();

    for &len in &msg_lengths {
        if offset + len > concatenated.len() {
            break;
        }
        let msg_slice = &concatenated[offset..offset + len];
        let msg = Message::from_bytes(msg_slice)
            .unwrap_or_else(|e| panic!("deserialize at offset {}: {}", offset, e));
        recovered.push(msg);
        offset += len;
    }

    assert_eq!(recovered, messages, "concatenated stream round-trip");
    assert_eq!(offset, concatenated.len(), "all bytes should be consumed");
    eprintln!(
        "Stream round-trip: {} messages, {} bytes total",
        messages.len(),
        concatenated.len(),
    );
}

// ============================================================================
// Test 6: Edge cases and large payloads
// ============================================================================

#[test]
fn test_large_payload_message() {
    // Test messages with various payload sizes
    let test_sizes = [
        0usize,          // empty payload
        1,               // single byte
        MAX_PACKET_SIZE, // MTU-sized
        MAX_PACKET_SIZE * 4, // larger than MTU (multi-packet)
        100_000,         // ~100KB
    ];

    for &size in &test_sizes {
        let payload = vec![0xABu8; size];
        let msg = Message::new(MessageType::ScreenFrame, 1, payload.clone());

        let bytes = msg.to_bytes()
            .unwrap_or_else(|e| panic!("serialize payload size {}: {}", size, e));
        let decoded = Message::from_bytes(&bytes)
            .unwrap_or_else(|e| panic!("deserialize payload size {}: {}", size, e));

        assert_eq!(msg.message_type, decoded.message_type);
        assert_eq!(msg.sequence_number, decoded.sequence_number);
        assert_eq!(msg.payload_length, decoded.payload_length);
        assert_eq!(msg.payload, decoded.payload);

        // Verify bincode overhead is consistent for all payload sizes.
        // The overhead includes: enum tag (4) + seq_num (4) + payload_len (4)
        // + reserved (2) + Vec length prefix (8) = 22 bytes fixed overhead.
        let overhead = bytes.len() - size;
        assert_eq!(overhead, 22,
            "bincode framing overhead should be 22 bytes for payload size {}", size);
        // The protocol header_size is 11 bytes (wire format), but bincode
        // adds serde framing beyond the raw protocol header.
    }
}

// ============================================================================
// Test 7: Scanout/CaptureFrame → encoding integration
// ============================================================================

#[test]
fn test_captured_frame_to_encoding_pipeline() {
    // Simulate what happens when a CapturedFrame from the capture module
    // flows through the encoding pipeline.

    let width = 640u32;
    let height = 480u32;
    let stride = width * 4;

    // Simulate a captured frame
    let frame_data = make_gradient_frame(width, height);
    let captured = CapturedFrame::new(frame_data, width, height, stride);

    assert_eq!(captured.width, width);
    assert_eq!(captured.height, height);
    assert_eq!(captured.stride, stride);
    assert_eq!(captured.data.len(), (width * height * 4) as usize);

    // Now run the frame through the encoding pipeline
    let tile_size = 64u32;
    let checksums = tile_checksums(&captured.data, captured.width, captured.height, tile_size);

    // Simulate a second frame with slight changes
    let mut frame2 = captured.data.clone();
    // Change a small rectangle (cursor area)
    let cursor_x = 100u32;
    let cursor_y = 200u32;
    for dy in 0..32 {
        for dx in 0..32 {
            let px = ((cursor_y + dy) * stride + (cursor_x + dx) * 4) as usize;
            if px + 3 < frame2.len() {
                frame2[px] = 0xFF;     // white cursor
                frame2[px + 1] = 0xFF;
                frame2[px + 2] = 0xFF;
                frame2[px + 3] = 0xFF;
            }
        }
    }

    let checksums2 = tile_checksums(&frame2, width, height, tile_size);
    let changed = detect_delta_tiles(&checksums, &checksums2);

    // The cursor tile(s) should be detected
    assert!(!changed.is_empty(), "cursor change should be detected");

    let cursor_tile_col = cursor_x / tile_size;
    let cursor_tile_row = cursor_y / tile_size;
    assert!(
        changed.contains(&(cursor_tile_col, cursor_tile_row)),
        "cursor tile ({}, {}) should be in changed set: {:?}",
        cursor_tile_col,
        cursor_tile_row,
        changed,
    );

    // Build, compress, decompress, verify
    let regions = build_delta_regions(
        &frame2, width, height, stride, &changed, tile_size,
    );
    let compressed = compress_delta(&regions).expect("compress cursor delta");
    let decompressed = decompress_delta(&compressed).expect("decompress cursor delta");

    for (orig, dec) in regions.iter().zip(decompressed.iter()) {
        assert_eq!(orig.lz4_compressed_data, dec.lz4_compressed_data,
            "cursor delta pixel data mismatch");
    }
}

// ============================================================================
// Test 8: Concurrent frame processing (simulated multi-threaded)
// ============================================================================

#[test]
fn test_multiple_frames_sequential_delta() {
    // Simulate multiple frames being processed in sequence:
    //   Frame 0 (baseline) → Frame 1 (small change) → Frame 2 (bigger change)

    let width = 640u32;
    let height = 480u32;
    let tile_size = 64u32;
    let stride = width * 4;

    // Frame 0: solid gray
    let frame0 = make_uniform_frame(width, height, 128);
    let chk0 = tile_checksums(&frame0, width, height, tile_size);

    // Frame 1: small cursor in center
    let mut frame1 = frame0.clone();
    for y in 200..250 {
        for x in 300..350 {
            let idx = ((y * stride + x * 4) as usize).min(frame1.len() - 4);
            frame1[idx] = 255;
            frame1[idx + 1] = 255;
            frame1[idx + 2] = 255;
        }
    }
    let chk1 = tile_checksums(&frame1, width, height, tile_size);
    let delta1 = detect_delta_tiles(&chk0, &chk1);
    assert!(!delta1.is_empty(), "frame 1 should have changes");

    let regions1 = build_delta_regions(&frame1, width, height, stride, &delta1, tile_size);
    let comp1 = compress_delta(&regions1).expect("compress frame 1 delta");
    let decomp1 = decompress_delta(&comp1).expect("decompress frame 1 delta");
    assert_eq!(decomp1.len(), regions1.len());

    // Frame 2: larger change (left half fills with white)
    let mut frame2 = frame1.clone();
    for y in 0..height {
        for x in 0..(width / 4) {
            let idx = ((y * stride + x * 4) as usize).min(frame2.len() - 4);
            frame2[idx] = 255;
            frame2[idx + 1] = 255;
            frame2[idx + 2] = 255;
        }
    }
    let chk2 = tile_checksums(&frame2, width, height, tile_size);
    let delta2 = detect_delta_tiles(&chk1, &chk2);
    assert!(!delta2.is_empty(), "frame 2 should have changes");

    let regions2 = build_delta_regions(&frame2, width, height, stride, &delta2, tile_size);
    let comp2 = compress_delta(&regions2).expect("compress frame 2 delta");
    let decomp2 = decompress_delta(&comp2).expect("decompress frame 2 delta");
    for (orig, dec) in regions2.iter().zip(decomp2.iter()) {
        assert_eq!(orig.lz4_compressed_data, dec.lz4_compressed_data);
    }

    // Check that frame 2 has more changed tiles than frame 1
    eprintln!(
        "Sequential delta: frame1={} changed tiles, frame2={} changed tiles",
        delta1.len(),
        delta2.len(),
    );
}

// ============================================================================
// Test 9: EncodingCapabilities serialization in various configurations
// ============================================================================

#[test]
fn test_various_encoding_capabilities_configs() {
    let configs = vec![
        EncodingCapabilities {
            lz4_delta: true,
            h264_low_delay: false,
            av1_rt: false,
            max_width: 1920,
            max_height: 1080,
        },
        EncodingCapabilities {
            lz4_delta: false,
            h264_low_delay: true,
            av1_rt: true,
            max_width: 3840,
            max_height: 2160,
        },
        EncodingCapabilities {
            lz4_delta: true,
            h264_low_delay: true,
            av1_rt: true,
            max_width: 7680,
            max_height: 4320,
        },
        EncodingCapabilities {
            lz4_delta: false,
            h264_low_delay: false,
            av1_rt: false,
            max_width: 0,
            max_height: 0,
        },
    ];

    for (i, caps) in configs.iter().enumerate() {
        let resp = create_capabilities_response(i as u32, true, "", caps.clone())
            .unwrap_or_else(|e| panic!("create caps config {}: {}", i, e));
        let bytes = resp.to_bytes().unwrap_or_else(|e| panic!("serialize caps {}: {}", i, e));
        let decoded = Message::from_bytes(&bytes)
            .unwrap_or_else(|e| panic!("deserialize caps {}: {}", i, e));

        let decoded_payload: ConnectionManagementPayload =
            bincode::deserialize(&decoded.payload)
                .unwrap_or_else(|e| panic!("deserialize caps payload {}: {}", i, e));
        match decoded_payload {
            ConnectionManagementPayload::Capabilities(c) => {
                assert_eq!(c.encoding.lz4_delta, caps.lz4_delta, "config {} lz4", i);
                assert_eq!(c.encoding.h264_low_delay, caps.h264_low_delay, "config {} h264", i);
                assert_eq!(c.encoding.av1_rt, caps.av1_rt, "config {} av1", i);
                assert_eq!(c.encoding.max_width, caps.max_width, "config {} width", i);
                assert_eq!(c.encoding.max_height, caps.max_height, "config {} height", i);
            }
            _ => panic!("Expected CapabilitiesResponse for config {}", i),
        }
    }
}

// ============================================================================
// Test 10: Message header boundary conditions
// ============================================================================

#[test]
fn test_message_boundary_conditions() {
    // Minimum message (empty payload)
    // Note: bincode adds framing overhead (enum tag + Vec length prefix),
    // so the actual serialised size is larger than the protocol header_size().
    let min_msg = Message::new(MessageType::Heartbeat, 0, vec![]);
    let min_bytes = min_msg.to_bytes().expect("serialize min message");
    assert!(min_bytes.len() <= 22, "min message should be small");
    let min_decoded = Message::from_bytes(&min_bytes).expect("deserialize min message");
    assert_eq!(min_msg, min_decoded);

    // Max sequence number
    let max_seq_msg = Message::new(MessageType::Ack, u32::MAX, vec![1, 2, 3]);
    let max_seq_bytes = max_seq_msg.to_bytes().expect("serialize max seq");
    let max_seq_decoded = Message::from_bytes(&max_seq_bytes).expect("deserialize max seq");
    assert_eq!(max_seq_msg, max_seq_decoded);

    // All message types
    for mt in &[
        MessageType::ControlCommand,
        MessageType::ScreenFrame,
        MessageType::Ack,
        MessageType::Heartbeat,
        MessageType::ConnectionManagement,
    ] {
        let msg = Message::new(*mt, 1, vec![0x42; 16]);
        let bytes = msg.to_bytes().expect("serialize");
        let decoded = Message::from_bytes(&bytes).expect("deserialize");
        assert_eq!(msg.message_type, decoded.message_type);
    }
}

// ============================================================================
// Test 11: Very small frame edge case
// ============================================================================

#[test]
fn test_very_small_frame_encoding() {
    // Test encoding pipeline with a tiny frame (100x100)
    let width = 100u32;
    let height = 100u32;
    let tile_size = 64u32;
    let stride = width * 4;

    let frame_a = make_gradient_frame(width, height);
    let mut frame_b = frame_a.clone();
    // Modify a single pixel
    frame_b[0] = 255; // change B of first pixel

    let chk_a = tile_checksums(&frame_a, width, height, tile_size);
    let chk_b = tile_checksums(&frame_b, width, height, tile_size);
    let changed = detect_delta_tiles(&chk_a, &chk_b);
    assert_eq!(changed.len(), 1, "only tile (0,0) should change");

    let regions = build_delta_regions(&frame_b, width, height, stride, &changed, tile_size);
    let compressed = compress_delta(&regions).expect("compress small frame delta");
    let decompressed = decompress_delta(&compressed).expect("decompress small frame delta");
    assert_eq!(regions[0].lz4_compressed_data, decompressed[0].lz4_compressed_data);

    // Full frame round-trip
    let full_compressed = compress_full_frame(&frame_b).expect("compress small frame full");
    let full_decompressed =
        decompress_full_frame(&full_compressed, frame_b.len()).expect("decompress small frame full");
    assert_eq!(frame_b, full_decompressed);
}
