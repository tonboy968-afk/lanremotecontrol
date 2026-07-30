//! LANRemoteControl Host Service
//!
//! Runs on the machine being controlled. Listens for incoming connections,
//! captures screen frames, and injects received input events.

fn main() {
    println!("LANRemoteControl Host Service");
    println!("=============================");
    println!("Listening on port: {}", lanremotecontrol_common::DEFAULT_PORT);
    println!("Use --help for available options.");
}
