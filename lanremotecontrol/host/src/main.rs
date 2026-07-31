//! LANRemoteControl Host Service (threaded: capture / send / receive decoupled)
//!
//! Architecture:
//!   - Capture thread: DXGI capture + LZ4 tile-delta encode → pushes `FrameToSend`
//!     into a bounded channel. Never touches the network, so it is never blocked
//!     by UDP send backlog (the root cause of the old single-thread stutter).
//!   - Send thread: drains the channel, chunks each frame and transmits via UDP.
//!     The 10055 backpressure (sleep-retry) lives here and only stalls THIS thread,
//!     not capture.
//!   - Main thread: receive loop + heartbeats + connection management + input
//!     injection. Fully independent of capture/send timing.

mod capture;
mod input;
mod net;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use lanremotecontrol_common::*;
use lanremotecontrol_common::capture::CaptureError;
use lanremotecontrol_common::encoding;

use net::{ConnectionManager, HeartbeatManager, UdpListener};

/// A fully-encoded frame handed from the capture thread to the send thread.
/// The send thread only chunks + transmits; it performs no encode work.
struct FrameToSend {
    msg_id: u32,
    seq: u32,
    width: u32,
    height: u32,
    chunk_type: MessageType,
    payload: Vec<u8>,
}

/// Max frames buffered between capture and send. Bounds memory if the send
/// thread ever falls behind the capture rate, and provides gentle backpressure.
const FRAME_CHANNEL_CAP: usize = 4;

/// Capture target interval (≈60fps cap). Capture is throttled to this so the
/// host doesn't spin at unbounded fps.
const FRAME_INTERVAL: Duration = Duration::from_millis(16);

/// Keyframe (full frame) cadence in frames.
const KEYFRAME_INTERVAL: u32 = 15;

fn main() {
    println!("LANRemoteControl Host Service (threaded)");
    println!("=============================");

    let listener = match UdpListener::bind(DEFAULT_PORT) {
        Ok(l) => {
            let addr = l.local_addr().expect("local_addr");
            println!("[OK] Listening on udp://{}", addr);
            Arc::new(l)
        }
        Err(e) => {
            eprintln!("[X] Failed to bind to port {}: {}", DEFAULT_PORT, e);
            std::process::exit(1);
        }
    };

    let conn_mgr: Arc<Mutex<ConnectionManager>> =
        Arc::new(Mutex::new(ConnectionManager::new()));
    let force_full_next = Arc::new(AtomicBool::new(false));
    let running = Arc::new(AtomicBool::new(true));

    let ctrlc_running = Arc::clone(&running);
    ctrlc::set_handler(move || {
        println!("\n[!] Shutdown signal received (Ctrl+C)");
        ctrlc_running.store(false, Ordering::SeqCst);
    })
    .expect("Error setting Ctrl+C handler");

    let (tx, rx) = sync_channel::<FrameToSend>(FRAME_CHANNEL_CAP);

    // --- Capture thread: capture + tile-delta encode, push to channel ---
    let capture_handle = {
        let conn_mgr = Arc::clone(&conn_mgr);
        let force_full_next = Arc::clone(&force_full_next);
        let running = Arc::clone(&running);
        thread::spawn(move || {
            capture_loop(conn_mgr, force_full_next, running, tx);
        })
    };

    // --- Send thread: drain channel, chunk + transmit over UDP ---
    let send_handle = {
        let listener = Arc::clone(&listener);
        let conn_mgr = Arc::clone(&conn_mgr);
        let running = Arc::clone(&running);
        thread::spawn(move || {
            send_loop(listener, conn_mgr, running, rx);
        })
    };

    println!("[i] Capture + send threads started. Waiting for connections ...\n");

    // --- Main thread: receive loop + heartbeats (never blocks on capture/send) ---
    let mut hb = HeartbeatManager::new(Duration::from_millis(HEARTBEAT_INTERVAL_MS));

    while running.load(Ordering::SeqCst) {
        match listener.receive_message() {
            Ok((msg, addr)) => match msg.message_type {
                MessageType::ConnectionManagement => {
                    let reply = {
                        let mut mgr = conn_mgr.lock().unwrap();
                        mgr.handle_message(msg, addr).ok().and_then(|r| r)
                    };
                    if let Some(reply) = reply {
                        if let Err(e) = listener.send_message(&reply, addr) {
                            eprintln!("[!] Failed to send reply to {}: {}", addr, e);
                        } else {
                            println!("[->] Sent capabilities response to {}", addr);
                        }
                    } else {
                        println!(
                            "[i] Connection management message from {} (active: {})",
                            addr,
                            conn_mgr.lock().unwrap().active_count()
                        );
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
                            eprintln!("[!] Invalid ControlCommand payload from {}: {}", addr, e)
                        }
                    }
                }
                MessageType::RequestKeyframe => {
                    force_full_next.store(true, Ordering::SeqCst);
                    println!("[i] Keyframe request from {} (decoder refresh)", addr);
                }
                _ => println!("[i] Received {:?} from {}", msg.message_type, addr),
            },
            Err(ref e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(e) => eprintln!("[!] Receive error: {}", e),
        }

        if hb.tick() {
            let seq = hb.current_seq();
            let addrs = conn_mgr.lock().unwrap().active_addrs();
            if !addrs.is_empty() {
                let hb_msg = create_heartbeat(seq);
                for &addr in &addrs {
                    if let Err(e) = listener.send_message(&hb_msg, addr) {
                        eprintln!("[!] Failed to send heartbeat to {}: {}", addr, e);
                    }
                }
                println!(
                    "[H] Heartbeat seq={} (active connections: {})",
                    seq,
                    addrs.len()
                );
            }
            if !hb.check_alive() {
                eprintln!("[!] Warning: {} heartbeats missed for a connection", seq);
            }
        }
    }

    println!("\n[i] Shutting down host service ...");
    capture_handle.join().ok();
    send_handle.join().ok();
    println!("[i] Host service shut down gracefully.");
}

/// Capture + encode loop. Runs on its own thread; never performs network I/O,
/// so it is immune to UDP send backlog (the old single-thread stutter root
/// cause). Encoded frames are handed to the send thread via the bounded channel.
fn capture_loop(
    conn_mgr: Arc<Mutex<ConnectionManager>>,
    force_full_next: Arc<AtomicBool>,
    running: Arc<AtomicBool>,
    tx: SyncSender<FrameToSend>,
) {
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

    let tile_size = encoding::DEFAULT_TILE_SIZE;
    let mut prev_checksums: Option<HashMap<(u32, u32), u32>> = None;
    let mut frame_seq: u32 = 0;
    let mut last_frame_time = Instant::now();

    while running.load(Ordering::SeqCst) {
        // Only capture when at least one client is connected.
        if conn_mgr.lock().unwrap().active_count() == 0 {
            thread::sleep(FRAME_INTERVAL);
            continue;
        }

        // Throttle to FRAME_INTERVAL so the host doesn't spin at unbounded fps.
        let elapsed = last_frame_time.elapsed();
        if elapsed < FRAME_INTERVAL {
            thread::sleep(FRAME_INTERVAL - elapsed);
            continue;
        }
        last_frame_time = Instant::now();

        let cap = match capture.as_mut() {
            Some(c) => c,
            None => {
                thread::sleep(Duration::from_millis(100));
                continue;
            }
        };

        let frame = match cap.capture_frame() {
            Ok(f) => f,
            Err(CaptureError::FrameAcquireFailed(_)) => continue,
            Err(CaptureError::DeviceLost) => {
                eprintln!("[!] DXGI device lost, reinitializing...");
                capture = capture::DxgiCapture::new().ok();
                prev_checksums = None;
                continue;
            }
            Err(e) => {
                eprintln!("[!] Capture error: {:?}", e);
                continue;
            }
        };

        let raw_size = frame.data.len();
        let width = frame.width;
        let height = frame.height;

        // Send at native resolution - no downscale (client scales via display).
        let current_checksums =
            encoding::compute_tile_checksums(&frame.data, width, height, tile_size);
        let force_full = force_full_next.swap(false, Ordering::SeqCst)
            || (frame_seq > 0 && frame_seq % KEYFRAME_INTERVAL == 0);

        let (compressed_data, frame_type): (Vec<u8>, &str) =
            if force_full || prev_checksums.is_none() {
                match encoding::compress_full_frame(&frame.data) {
                    Ok(c) => {
                        prev_checksums = Some(current_checksums);
                        (c, "full")
                    }
                    Err(e) => {
                        eprintln!("[!] compress_full_frame failed: {}", e);
                        continue;
                    }
                }
            } else {
                let prev = prev_checksums.as_ref().unwrap();
                let changed = encoding::detect_delta_tiles(prev, &current_checksums);
                if changed.is_empty() {
                    // No changes since last frame; skip entirely.
                    continue;
                }
                let total = encoding::total_tile_count(width, height, tile_size);
                if encoding::should_send_full_frame(changed.len(), total) {
                    match encoding::compress_full_frame(&frame.data) {
                        Ok(c) => {
                            prev_checksums = Some(current_checksums);
                            (c, "full")
                        }
                        Err(e) => {
                            eprintln!("[!] compress_full_frame failed: {}", e);
                            continue;
                        }
                    }
                } else {
                    match encoding::compress_delta_tiles(
                        &frame.data,
                        width,
                        height,
                        tile_size,
                        &changed,
                    ) {
                        Ok(c) => {
                            prev_checksums = Some(current_checksums);
                            (c, "delta")
                        }
                        Err(_) => match encoding::compress_full_frame(&frame.data) {
                            Ok(c) => {
                                prev_checksums = Some(current_checksums);
                                (c, "full")
                            }
                            Err(e) => {
                                eprintln!("[!] compress_full_frame failed: {}", e);
                                continue;
                            }
                        },
                    }
                }
            };

        frame_seq = frame_seq.wrapping_add(1);
        let chunk_type = if frame_type == "delta" {
            MessageType::ScreenFrameChunkDelta
        } else {
            MessageType::ScreenFrameChunk
        };

        let frame_to_send = FrameToSend {
            msg_id: frame_seq,
            seq: frame_seq,
            width,
            height,
            chunk_type,
            payload: compressed_data,
        };

        let payload_len = frame_to_send.payload.len();

        // Hand off to the send thread. Blocks only if the channel is full (send
        // thread momentarily behind); capture is otherwise never blocked by
        // network I/O. This is the core of the thread-decoupling fix.
        if tx.send(frame_to_send).is_err() {
            // Send thread gone — exit capture loop.
            break;
        }

        if frame_seq <= 5 || frame_seq % 60 == 0 {
            let ratio = raw_size as f64 / payload_len.max(1) as f64;
            println!(
                "[F] Frame #{}: {}x{} px, {} bytes (LZ4 {}, {:.1}:1) -> send thread",
                frame_seq,
                width,
                height,
                payload_len,
                frame_type,
                ratio,
            );
        }
    }
}

/// Send loop. Runs on its own thread; drains the channel and transmits each
/// frame via `UdpListener::send_fragmented`. The 10055 backpressure
/// (sleep-retry on a full OS send buffer) only stalls this thread, never the
/// capture thread — so a slow client degrades to dropped frames (recovered by
/// DCC) instead of freezing the host.
fn send_loop(
    listener: Arc<UdpListener>,
    conn_mgr: Arc<Mutex<ConnectionManager>>,
    running: Arc<AtomicBool>,
    rx: Receiver<FrameToSend>,
) {
    while running.load(Ordering::SeqCst) {
        let frame = match rx.recv() {
            Ok(f) => f,
            Err(_) => break, // channel closed (capture thread exited)
        };
        let addrs = conn_mgr.lock().unwrap().active_addrs();
        if addrs.is_empty() {
            // No clients at send time — frame is obsolete, drop it.
            continue;
        }
        for &addr in &addrs {
            if let Err(e) = listener.send_fragmented(
                frame.msg_id,
                &frame.payload,
                frame.seq,
                addr,
                frame.width,
                frame.height,
                frame.chunk_type,
            ) {
                eprintln!(
                    "[!] Failed to send frame #{} to {}: {}",
                    frame.msg_id, addr, e
                );
            }
        }
    }
}
