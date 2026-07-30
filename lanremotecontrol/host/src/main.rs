//! LANRemoteControl Host Service
//!
//! Runs on the machine being controlled. Listens for incoming UDP connections,
//! manages the connection handshake, and maintains heartbeats.

mod net;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use lanremotecontrol_common::*;

use net::{ConnectionManager, HeartbeatManager, UdpListener};

fn main() {
    println!("LANRemoteControl Host Service");
    println!("=============================");
    println!("Press Ctrl+C to stop.\n");

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

    println!("[i] Waiting for connections ...");

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
            // Send heartbeat to all active connections
            // (In a real implementation we would iterate active_connections and
            //  send the heartbeat via `listener.send_message(&hb_msg, addr)`;
            //  for now we just print a status line.)
            if conn_mgr.active_count() > 0 {
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
    }

    println!("\n[i] Host service shut down gracefully.");
}
