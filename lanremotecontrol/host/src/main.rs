//! LANRemoteControl Host Service
//!
//! Runs on the machine being controlled. Listens for incoming UDP connections,
//! manages the connection handshake, and maintains heartbeats.

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

    // ── Initialize persistent screen capture ───────────────────────────
    let mut capture = match capture::DxgiCapture::new() {
        Ok(cap) => {
            println!("[✓] DXGI capture initialised");
            println!("[i] {}", cap.display_info());
            Some(cap)
        }
        Err(e) => {
            eprintln!("[i] Screen capture unavailable: {}", e);
            #[cfg(not(windows))]
            eprintln!("[i] DXGI is a Windows-only API (current platform is not Windows)");
            eprintln!("[i] Proceeding without screen capture — no video frames will be sent.");
            None
        }
    };

    println!("\nPress Ctrl+C to stop.\n");

    // ── Bind UDP listener ────────────────────────────────────────────────
    let listener = match UdpListener::bind(DEFAULT_PORT) {
        Ok(l) => {
            let addr = l.local_addr().expect("local_addr");
            println!("[✓] Listening on udp://{}", addr);
            l
        }
        Err(e) => {
            eprintln!("[✗] Failed to bind to port {}: {}", DEFAULT_PORT, e);
            std::process::exit(1);
        }
    };

    let mut conn_mgr = ConnectionManager::new();
    let mut hb = HeartbeatManager::new(Duration::from_millis(HEARTBEAT_INTERVAL_MS));
    let mut frame_seq: u32 = 0;

    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();

    // ── Register Ctrl+C handler ──────────────────────────────────────────
    // We use a simple thread-based approach (cross-platform) that blocks on
    // stdin.  When the user presses Enter, the service shuts down gracefully.
    // For Ctrl+C specifically we rely on the default OS behaviour to
    // terminate the process; the flag lets us catch it where possible.
    ctrlc::set_handler(move || {
        println!("\n[!] Shutdown signal received (Ctrl+C)");
        r.store(false, Ordering::SeqCst);
    })
    .expect("Error setting Ctrl+C handler");

    println!("[i] Waiting for connections ...\n");

    // ── Main event loop ──────────────────────────────────────────────────
    while running.load(Ordering::SeqCst) {
        // Try to receive a message (read timeout is 100 ms)
        match listener.receive_message() {
            Ok((msg, addr)) => {
                match msg.message_type {
                    MessageType::ConnectionManagement => {
                        match conn_mgr.handle_message(msg, addr) {
                            Ok(Some(reply)) => {
                                if let Err(e) = listener.send_message(&reply, addr) {
                                    eprintln!("[!] Failed to send reply to {}: {}", addr, e);
                                } else {
                                    println!("[→] Sent capabilities response to {}", addr);
                                }
                            }
                            Ok(None) => {
                                // Confirm or Teardown processed, no reply needed
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
                        // Client sent a heartbeat; send back an ACK
                        let ack = create_ack(msg.sequence_number);
                        if let Err(e) = listener.send_message(&ack, addr) {
                            eprintln!("[!] Failed to send ACK to {}: {}", addr, e);
                        }
                    }
                    MessageType::Ack => {
                        // Host received an ACK for a heartbeat it sent
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
                        // Client sent a keyboard/mouse input command
                        match bincode::deserialize::<ControlCommandPayload>(&msg.payload) {
                            Ok(cmd) => {
                                if let Err(e) = input::InputInjector::inject(&cmd) {
                                    eprintln!("[!] Input injection failed from {}: {}", addr, e);
                                }
                                // Send ACK per protocol requirement
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
                    _ => {
                        println!("[i] Received {:?} from {}", msg.message_type, addr);
                    }
                }
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock
                || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                // Normal timeout — nothing to receive, continue loop
            }
            Err(e) => {
                eprintln!("[!] Receive error: {}", e);
            }
        }

        // ── Heartbeat tick ───────────────────────────────────────────────
        if hb.tick() {
            let seq = hb.current_seq();
            if conn_mgr.active_count() > 0 {
                // 真正发送心跳消息到所有活跃连接
                let hb_msg = create_heartbeat(seq);
                for &addr in &conn_mgr.active_addrs() {
                    if let Err(e) = listener.send_message(&hb_msg, addr) {
                        eprintln!("[!] Failed to send heartbeat to {}: {}", addr, e);
                    }
                }
                println!(
                    "[♥] Heartbeat seq={} (active connections: {})",
                    seq,
                    conn_mgr.active_count()
                );
            }
            if !hb.check_alive() {
                eprintln!(
                    "[!] Warning: {} heartbeats missed for a connection",
                    seq
                );
            }
        }

        // ── Screen capture and frame broadcast ───────────────────────────
        if conn_mgr.active_count() > 0 {
            if let Some(ref mut cap) = capture {
                match cap.capture_frame() {
                    Ok(frame) => {
                        let raw_size = frame.data.len();
                        match encoding::compress_full_frame(&frame.data) {
                            Ok(compressed_data) => {
                                frame_seq = frame_seq.wrapping_add(1);
                                let msg_id = frame_seq;
                                for &addr in &conn_mgr.active_addrs() {
                                    if let Err(e) = listener.send_fragmented(
                                        msg_id,
                                        &compressed_data,
                                        frame_seq,
                                        addr,
                                        frame.width,
                                        frame.height,
                                    ) {
                                        eprintln!(
                                            "[!] Failed to send frame to {}: {}",
                                            addr, e
                                        );
                                    }
                                }
                                // Log first few frames + every 60 frames
                                if frame_seq <= 5 || frame_seq % 60 == 0 {
                                    let ratio = raw_size as f64 / compressed_data.len().max(1) as f64;
                                    println!(
                                        "[📷] Frame #{}: {}x{} px, {} bytes (LZ4, {:.1}:1) → {} client(s)",
                                        frame_seq,
                                        frame.width,
                                        frame.height,
                                        compressed_data.len(),
                                        ratio,
                                        conn_mgr.active_count(),
                                    );
                                }
                            }
                            Err(e) => {
                                eprintln!("[!] LZ4 compress failed: {}", e);
                                // Fallback: skip frame rather than send nothing
                            }
                        }
                    }
                    Err(CaptureError::FrameAcquireFailed(_)) => {
                        // Timeout — no new frame, silently skip
                    }
                    Err(CaptureError::DeviceLost) => {
                        eprintln!("[!] DXGI device lost, reinitializing...");
                        capture = capture::DxgiCapture::new().ok();
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
