//! LANRemoteControl Client Application
//!
//! Runs on the controlling machine. Connects to the host service,
//! forwards input events, and displays the remote screen.

fn main() {
    println!("LANRemoteControl Client Application");
    println!("===================================");
    println!("Connect to host at port: {}", lanremotecontrol_common::DEFAULT_PORT);
    println!("Use --help for available options.");
}
