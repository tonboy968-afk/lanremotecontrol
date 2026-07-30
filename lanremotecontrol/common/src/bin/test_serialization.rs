use bincode;
use lanremotecontrol_common::*;

fn main() {
    // Test Message serialization
    let msg = Message::new(MessageType::Heartbeat, 42, vec![1, 2, 3]);
    let bytes = bincode::serialize(&msg).unwrap();
    println!("Heartbeat Message: {} bytes", bytes.len());
    println!("  Raw: {:?}", bytes);
    println!("  Byte 0 (msg_type): 0x{:02x}", bytes[0]);
    
    // Test ConnectionManagementPayload::Request
    let req = ConnectionManagementPayload::Request(ConnectionRequest {
        auth_token: "".to_string(),
        protocol_version: 1,
    });
    let req_bytes = bincode::serialize(&req).unwrap();
    println!("\nConnRequest payload: {} bytes", req_bytes.len());
    println!("  Raw: {:?}", req_bytes);
    
    // Full message with ConnectionRequest
    let msg2 = Message::new(MessageType::ConnectionManagement, 1, req_bytes);
    let bytes2 = bincode::serialize(&msg2).unwrap();
    println!("\nFull ConnRequest Message: {} bytes", bytes2.len());
    println!("  Raw: {:?}", bytes2);
    
    // Also test what a client sends
    let req_msg = create_connection_request(1, "", 1).unwrap();
    let req_msg_bytes = req_msg.to_bytes().unwrap();
    println!("\ncreate_connection_request: {} bytes", req_msg_bytes.len());
    println!("  Raw: {:?}", req_msg_bytes);
}
