//! LANRemoteControl Host Service

mod capture;
mod input;
mod net;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use lanremotecontrol_common::*;
use lanremotecontrol_common::capture::CaptureError;
use lanremotecontrol_common::encoding;

use net::{ConnectionManager, HeartbeatManager, UdpListener};

fn main() {
    println!("LANRemoteControl Host Service");
    println!("=============================");

    let mut capture = match capture::DxgiCapture::new() {
        Ok(cap) => {
            println!("[OK] DXGI capture initialised");
            println!("[i] {}", cap.display_info());
            Some(cap)
        }
        Err(e) => {
            eprintln!("[i] Screen capture unavailable: {}", e);
            None
        }
    };

    println!("\nPress Ctrl+C to stop.\n");

    let listener = match UdpListener::bind(DEFAULT_PORT) {
        Ok(l) => {
            let addr = l.local_addr().expect("local_addr");
            println!("[OK] Listening on udp://{}", addr);
            l
        }
        Err(e) => {
            eprintln!("[X] Failed to bind to port {}: {}", DEFAULT_PORT, e);
            std::process::exit(1);
        }
    };

    let mut conn_mgr = ConnectionManager::new();
    let mut hb = HeartbeatManager::new(Duration::from_millis(HEARTBEAT_INTERVAL_MS));
    let mut frame_seq: u32 = 0;
    let mut last_frame_time = std::time::Instant::now();
    const FRAME_INTERVAL: Duration = Duration::from_millis(16); // 60fps cap

    // Tile-delta state
    let mut prev_checksums: Option<std::collections::HashMap<(u32, u32), u32>> = None;
    let mut force_full_next = false;

    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();

    ctrlc::set_handler(move || {
        println!("\n[!] Shutdown signal received (Ctrl+C)");
        r.store(false, Ordering::SeqCst);
    })
    .expect("Error setting Ctrl+C handler");

    println!("[i] Waiting for connections ...\n");

    while running.load(Ordering::SeqCst) {
        match listener.receive_message() {
            Ok((msg, addr)) => {
                match msg.message_type {
                    MessageType::ConnectionManagement => {
                        match conn_mgr.handle_message(msg, addr) {
                            Ok(Some(reply)) => {
                                if let Err(e) = listener.send_message(&reply, addr) {
                                    eprintln!("[!] Failed to send reply to {}: {}", addr, e);
                                } else {
                                    println!("[->] Sent capabilities response to {}", addr);
                                }
                            }
                            Ok(None) => {
                                println!(
                                    "[i] Connection management message from {} (active: {})",
                                    addr,
                                    conn_mgr.active_count()
                                );
                            }
                            Err(e) => {
                                eprintln!("[!] Error handling connection mgmt msg: {}", e);
                            }
                        }
                    }
                    MessageType::Heartbeat => {
                        let ack = create_ack(msg.sequence_number);
                        if let Err(e) = listener.send_message(&ack, addr) {
                            eprintln!("[!] Failed to send ACK to {}: {}", addr, e);
                        }
                    }
                    MessageType::Ack => {
                        let seq_bytes = &msg.payload;
                        if seq_bytes.len() >= 4 {
                            let seq = u32::from_le_bytes([
                                seq_bytes[0],
                                seq_bytes[1],
                                seq_bytes[2],
                                seq_bytes[3],
                            ]);
                            hb.received_ack(seq);
                        }
                    }
                    MessageType::ControlCommand => {
                        match bincode::deserialize::<ControlCommandPayload>(&msg.payload) {
                            Ok(cmd) => {
                                if let Err(e) = input::InputInjector::inject(&cmd) {
                                    eprintln!("[!] Input injection failed from {}: {}", addr, e);
                                }
                                let ack = create_ack(msg.sequence_number);
                                if let Err(e) = listener.send_message(&ack, addr) {
                                    eprintln!("[!] Failed to send ACK to {}: {}", addr, e);
                                }
                            }
                            Err(e) => {
                                eprintln!("[!] Invalid ControlCommand payload from {}: {}", addr, e);
                            }
                        }
                    }
                    MessageType::RequestKeyframe => {
                        force_full_next = true;
                        println!("[i] Keyframe request from {} (decoder refresh)", addr);
                    }
                    _ => {
                        println!("[i] Received {:?} from {}", msg.message_type, addr);
                    }
                }
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock
                || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                // Normal timeout
            }
            Err(e) => {
                eprintln!("[!] Receive error: {}", e);
            }
        }

        // Drip any queued full frame (keyframe) a few chunks per tick so the
        // capture thread is never blocked by a large multi-MB send.
        if conn_mgr.active_count() > 0 {
            if let Err(e) = listener.pump_queued(128, &conn_mgr.active_addrs()) {
                eprintln!("[!] Failed to pump queued frame: {}", e);
            }
        }

        // Heartbeat tick
        if hb.tick() {
            let seq = hb.current_seq();
            if conn_mgr.active_count() > 0 {
                let hb_msg = create_heartbeat(seq);
                for &addr in &conn_mgr.active_addrs() {
                    if let Err(e) = listener.send_message(&hb_msg, addr) {
                        eprintln!("[!] Failed to send heartbeat to {}: {}", addr, e);
                    }
                }
                println!(
                    "[H] Heartbeat seq={} (active connections: {})",
                    seq,
                    conn_mgr.active_count()
                );
            }
            if !hb.check_alive() {
                eprintln!("[!] Warning: {} heartbeats missed for a connection", seq);
            }
        }

        // Screen capture and frame broadcast (tile-delta encoding)
        if conn_mgr.active_count() > 0 && last_frame_time.elapsed() >= FRAME_INTERVAL {
            last_frame_time = std::time::Instant::now();
            if let Some(ref mut cap) = capture {
                match cap.capture_frame() {
                    Ok(frame) => {
                        let raw_size = frame.data.len();
                        let width = frame.width;
                        let height = frame.height;

                        let t0 = std::time::Instant::now();

                        // Send at native resolution - no downscale (client scales via display)
                        let (send_data, send_w, send_h) = (frame.data, width, height);

                        let t_downscale = t0.elapsed();

                        // Tile-delta encoding
                        let t1 = std::time::Instant::now();
                        let tile_size = encoding::DEFAULT_TILE_SIZE;
                        let current_checksums = encoding::compute_tile_checksums(
                            &send_data, send_w, send_h, tile_size,
                        );

                        // Force full frame every 15 frames for keyframe recovery
                        // (or immediately when client requests a decoder refresh)
                        let force_full = force_full_next || (frame_seq > 0 && frame_seq % 15 == 0);
                        if force_full_next {
                            force_full_next = false;
                        }

                        let (compressed_data, frame_type, changed_count, total_tiles) =
                            if force_full || prev_checksums.is_none() {
                                let c = encoding::compress_full_frame(&send_data)
                                    .expect("compress_full_frame");
                                prev_checksums = Some(current_checksums);
                                (c, "full", 0u32, 0u32)
                            } else {
                                let prev = prev_checksums.as_ref().unwrap();
                                let changed = encoding::detect_delta_tiles(prev, &current_checksums);
                                if changed.is_empty() {
                                    // No changes, skip
                                    continue;
                                }
                                let total = encoding::total_tile_count(send_w, send_h, tile_size);
                                if encoding::should_send_full_frame(changed.len(), total) {
                                    let c = encoding::compress_full_frame(&send_data)
                                        .expect("compress_full_frame");
                                    prev_checksums = Some(current_checksums);
                                    (c, "full", changed.len() as u32, total as u32)
                                } else {
                                    match encoding::compress_delta_tiles(
                                        &send_data, send_w, send_h, tile_size, &changed,
                                    ) {
                                        Ok(c) => {
                                            prev_checksums = Some(current_checksums);
                                            (c, "delta", changed.len() as u32, total as u32)
                                        }
                                        Err(_) => {
                                            let c = encoding::compress_full_frame(&send_data)
                                                .expect("compress_full_frame");
                                            prev_checksums = Some(current_checksums);
                                            (c, "full", changed.len() as u32, total as u32)
                                        }
                                    }
                                }
                            };

                        let t_encode = t1.elapsed();
                        let t_checksum = t_encode; // combined for now

                        let t2 = std::time::Instant::now();
                        frame_seq = frame_seq.wrapping_add(1);
                        let msg_id = frame_seq;
                        let chunk_type = if frame_type == "delta" {
                            MessageType::ScreenFrameChunkDelta
                        } else {
                            MessageType::ScreenFrameChunk
                        };
                        if frame_type == "full" {
                            // Queue the large keyframe for interleaved sending so
                            // capture is never blocked by a multi-MB send.
                            listener.enqueue_frame(
                                msg_id,
                                &compressed_data,
                                frame_seq,
                                send_w,
                                send_h,
                                chunk_type,
                            );
                        } else {
                            for &addr in &conn_mgr.active_addrs() {
                                if let Err(e) = listener.send_fragmented(
                                    msg_id,
                                    &compressed_data,
                                    frame_seq,
                                    addr,
                                    send_w,
                                    send_h,
                                    chunk_type,
                                ) {
                                    eprintln!("[!] Failed to send frame to {}: {}", addr, e);
                                }
                            }
                        }
                        let t_send = t2.elapsed();

                        // Log first 5 + every 60 frames
                        if frame_seq <= 5 || frame_seq % 60 == 0 {
                            let ratio = raw_size as f64 / compressed_data.len().max(1) as f64;
                            println!(
                                "[F] Frame #{}: {}x{} px, {} bytes (LZ4 {}, {:.1}:1) -> {} client(s)",
                                frame_seq,
                                send_w,
                                send_h,
                                compressed_data.len(),
                                frame_type,
                                ratio,
                                conn_mgr.active_count(),
                            );
                            println!(
                                "    timing: downscale={:.1}ms encode={:.1}ms send={:.1}ms",
                                t_downscale.as_secs_f64() * 1000.0,
                                t_checksum.as_secs_f64() * 1000.0,
                                t_send.as_secs_f64() * 1000.0,
                            );
                        }
                    }
                    Err(CaptureError::FrameAcquireFailed(_)) => {
                        // Timeout - no new frame
                    }
                    Err(CaptureError::DeviceLost) => {
                        eprintln!("[!] DXGI device lost, reinitializing...");
                        capture = capture::DxgiCapture::new().ok();
                        prev_checksums = None;
                    }
                    Err(e) => {
                        eprintln!("[!] Capture error: {:?}", e);
                    }
                }
            }
        }
    }

    println!("\n[i] Host service shut down gracefully.");
}

/// Bilinear downscale BGRA data. Preserves text clarity much better than nearest-neighbor.
fn downscale_bgra(
    src: &[u8],
    src_w: u32,
    src_h: u32,
    dst_w: u32,
    dst_h: u32,
) -> (Vec<u8>, u32, u32) {
    if src_w == dst_w && src_h == dst_h {
        return (src.to_vec(), dst_w, dst_h);
    }

    let src_stride = (src_w * 4) as usize;
    let dst_stride = (dst_w * 4) as usize;
    let mut dst = vec![0u8; (dst_stride * dst_h as usize)];

    let x_ratio = src_w as f64 / dst_w as f64;
    let y_ratio = src_h as f64 / dst_h as f64;

    for dy in 0..dst_h as usize {
        let src_y = dy as f64 * y_ratio;
        let sy0 = src_y.floor() as usize;
        let sy1 = (sy0 + 1).min(src_h as usize - 1);
        let wy = src_y - sy0 as f64;

        let dst_row_start = dy * dst_stride;

        for dx in 0..dst_w as usize {
            let src_x = dx as f64 * x_ratio;
            let sx0 = src_x.floor() as usize;
            let sx1 = (sx0 + 1).min(src_w as usize - 1);
            let wx = src_x - sx0 as f64;

            let dst_off = dst_row_start + dx * 4;

            let p00 = sy0 * src_stride + sx0 * 4;
            let p01 = sy0 * src_stride + sx1 * 4;
            let p10 = sy1 * src_stride + sx0 * 4;
            let p11 = sy1 * src_stride + sx1 * 4;

            for c in 0..4 {
                let v00 = src[p00 + c] as f64;
                let v01 = src[p01 + c] as f64;
                let v10 = src[p10 + c] as f64;
                let v11 = src[p11 + c] as f64;

                let top = v00 * (1.0 - wx) + v01 * wx;
                let bot = v10 * (1.0 - wx) + v11 * wx;
                let val = top * (1.0 - wy) + bot * wy;

                dst[dst_off + c] = val.round().clamp(0.0, 255.0) as u8;
            }
        }
    }

    (dst, dst_w, dst_h)
}