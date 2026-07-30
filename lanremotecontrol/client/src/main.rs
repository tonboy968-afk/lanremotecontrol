//! LANRemoteControl Client Application
//!
//! Runs on the controlling machine. Connects to the host service using the
//! three-way handshake (request → capabilities → confirm) and reports the
//! result.

mod input;
mod net;

use std::env;

use lanremotecontrol_common::*;

use net::{HandshakeClient, HandshakeError, UdpClient};

fn main() {
    println!("LANRemoteControl Client Application");
    println!("===================================\n");

    // ── Parse command-line arguments ─────────────────────────────────────
    let args: Vec<String> = env::args().collect();
    let host_ip = if args.len() > 1 {
        &args[1]
    } else {
        "127.0.0.1"
    };
    let port: u16 = if args.len() > 2 {
        args[2].parse().unwrap_or(DEFAULT_PORT)
    } else {
        DEFAULT_PORT
    };

    println!("[i] Connecting to {}:{} ...", host_ip, port);

    // ── Create UDP client ────────────────────────────────────────────────
    let client = match UdpClient::connect(host_ip, port) {
        Ok(c) => {
            println!(
                "[✓] UDP socket bound to {}",
                c.local_addr().expect("local_addr")
            );
            c
        }
        Err(e) => {
            eprintln!("[✗] Failed to connect to {}:{}: {}", host_ip, port, e);
            std::process::exit(1);
        }
    };

    // ── Perform handshake ────────────────────────────────────────────────
    println!("[i] Performing handshake (request → capabilities → confirm) ...");
    match HandshakeClient::perform_handshake(&client, "", 1) {
        Ok(caps) => {
            println!("\n[✓] Handshake successful!");
            println!("    ├─ Encoding: LZ4={}, H264={}, AV1={}", 
                caps.encoding.lz4_delta,
                caps.encoding.h264_low_delay,
                caps.encoding.av1_rt);
            println!("    └─ Max resolution: {}x{}", 
                caps.encoding.max_width,
                caps.encoding.max_height);
        }
        Err(e) => {
            match &e {
                HandshakeError::Timeout => {
                    eprintln!(
                        "[✗] Handshake timed out — host at {}:{} may be unreachable",
                        host_ip, port
                    );
                }
                HandshakeError::Rejected(reason) => {
                    eprintln!("[✗] Connection rejected: {}", reason);
                }
                _ => {
                    eprintln!("[✗] Handshake failed: {}", e);
                }
            }
            std::process::exit(1);
        }
    }
}
