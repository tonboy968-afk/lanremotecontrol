//! # LANRemoteControl Common Library
//!
//! Shared message types, protocol constants, session state, and serialization
//! helpers for the LANRemoteControl software.

use serde::{Deserialize, Serialize};

// Public sub-modules for screen capture abstractions and encoding.
pub mod capture;
pub mod encoding;
pub mod hevc;

// ============================================================================
// Protocol Constants
// ============================================================================

/// Default port for the host service to listen on.
pub const DEFAULT_PORT: u16 = 50000;

/// Interval (in milliseconds) between heartbeat messages.
pub const HEARTBEAT_INTERVAL_MS: u64 = 1000;

/// Maximum packet payload size (MTU-safe: 1500 - 20 IP - 8 UDP - 11 header).
pub const MAX_PACKET_SIZE: usize = 1400;

/// Timeout (in milliseconds) waiting for an ACK before retransmitting.
pub const ACK_TIMEOUT_MS: u64 = 50;

/// Maximum number of retransmissions for a message requiring ACK.
pub const MAX_RETRANSMIT: u32 = 3;

/// Maximum data bytes per screen frame chunk (fits safely in one UDP packet).
///
/// At 1400 MTU, the actual wire size of a chunk message is:
///   Message header (~22 bytes) + ScreenFrameChunk header (~32 bytes) + data.
/// 1320 + 54 ≈ 1374 bytes, well under the typical 1472 byte UDP payload limit.
pub const SCREEN_FRAME_CHUNK_DATA_SIZE: usize = 1320;

// ============================================================================
// Message Type Enum
// ============================================================================

/// Protocol message types, matching the design in docs/NETWORK_PROTOCOL.md.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum MessageType {
    /// Client-to-host input events (keyboard, mouse). Requires ACK.
    ControlCommand = 0x01,
    /// Host-to-client screen frame data (delta or full frame).
    ScreenFrame = 0x02,
    /// Acknowledgment for a specific sequence number.
    Ack = 0x03,
    /// Keep-alive heartbeat, sent by both sides.
    Heartbeat = 0x04,
    /// Connection management (initiation, capabilities exchange, teardown).
    ConnectionManagement = 0x05,
    /// Chunk of a fragmented screen frame (large frames spanning multiple packets).
    ScreenFrameChunk = 0x06,
}

impl MessageType {
    /// Create from a raw byte value. Returns `None` for unknown values.
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            0x01 => Some(Self::ControlCommand),
            0x02 => Some(Self::ScreenFrame),
            0x03 => Some(Self::Ack),
            0x04 => Some(Self::Heartbeat),
            0x05 => Some(Self::ConnectionManagement),
            0x06 => Some(Self::ScreenFrameChunk),
            _ => None,
        }
    }
}

// ============================================================================
// Core Message Struct
// ============================================================================

/// A single protocol message on the wire, following the binary format from
/// the network protocol design.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    /// Message type identifier (1 byte on the wire).
    pub message_type: MessageType,
    /// Monotonically increasing sequence number (4 bytes on the wire).
    pub sequence_number: u32,
    /// Length of the payload in bytes (4 bytes on the wire).
    pub payload_length: u32,
    /// Reserved field (2 bytes on the wire). Must be 0.
    pub reserved: u16,
    /// Message-specific payload data.
    pub payload: Vec<u8>,
}

impl Message {
    /// Create a new `Message` with the given type, sequence number, and
    /// payload. Automatically sets `payload_length`.
    pub fn new(message_type: MessageType, sequence_number: u32, payload: Vec<u8>) -> Self {
        let payload_length = payload.len() as u32;
        Self {
            message_type,
            sequence_number,
            payload_length,
            reserved: 0,
            payload,
        }
    }

    /// Serialize this message to a byte vector using bincode.
    pub fn to_bytes(&self) -> Result<Vec<u8>, bincode::Error> {
        bincode::serialize(self)
    }

    /// Deserialize a message from a byte slice using bincode.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, bincode::Error> {
        bincode::deserialize(bytes)
    }

    /// Maximum serialised size of an empty Message (header only).
    pub const fn header_size() -> usize {
        // MessageType(1) + sequence_number(4) + payload_length(4) + reserved(2)
        11
    }
}

// ============================================================================
// Control Command Sub-types
// ============================================================================

/// Keyboard event payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeyEvent {
    /// Virtual key code or scan code.
    pub key_code: u32,
    /// True = key pressed, False = key released.
    pub pressed: bool,
    /// Modifier flags (bit 0=Shift, bit 1=Ctrl, bit 2=Alt, bit 3=Win).
    pub modifiers: u8,
    /// Client-side timestamp in microseconds.
    pub timestamp_us: u64,
}

/// Mouse absolute or relative move event payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MouseMoveEvent {
    /// Delta X (relative movement, or absolute X if abs_coords is true).
    pub dx: i32,
    /// Delta Y (relative movement, or absolute Y if abs_coords is true).
    pub dy: i32,
    /// If true, dx/dy are absolute screen coordinates; otherwise deltas.
    pub abs_coords: bool,
    /// Client-side timestamp in microseconds.
    pub timestamp_us: u64,
}

/// Mouse button event payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MouseButtonEvent {
    /// Button identifier: 0=left, 1=right, 2=middle, 3=X1, 4=X2.
    pub button: u8,
    /// True = button pressed, False = button released.
    pub pressed: bool,
    /// Absolute cursor X at the time of the event.
    pub x: i32,
    /// Absolute cursor Y at the time of the event.
    pub y: i32,
    /// Client-side timestamp in microseconds.
    pub timestamp_us: u64,
}

/// Scroll / wheel event payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScrollEvent {
    /// Horizontal scroll delta (positive = right).
    pub delta_x: i32,
    /// Vertical scroll delta (positive = up).
    pub delta_y: i32,
    /// Client-side timestamp in microseconds.
    pub timestamp_us: u64,
}

/// Union of all control command payloads.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ControlCommandPayload {
    Key(KeyEvent),
    MouseMove(MouseMoveEvent),
    MouseButton(MouseButtonEvent),
    Scroll(ScrollEvent),
}

// ============================================================================
// Connection Management Sub-types
// ============================================================================

/// Encoding capabilities offered by the host.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EncodingCapabilities {
    /// True if LZ4 raw delta compression is supported.
    pub lz4_delta: bool,
    /// True if H.264 low-delay encoding is supported.
    pub h264_low_delay: bool,
    /// True if AV1 real-time encoding is supported.
    pub av1_rt: bool,
    /// Maximum screen width in pixels the host can capture.
    pub max_width: u32,
    /// Maximum screen height in pixels the host can capture.
    pub max_height: u32,
}

/// Connection request sent from client to host.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConnectionRequest {
    /// Optional authentication token / PIN (empty if not used).
    pub auth_token: String,
    /// Protocol version the client supports.
    pub protocol_version: u32,
}

/// Capabilities response sent from host to client.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CapabilitiesResponse {
    /// Whether the connection request was accepted.
    pub accepted: bool,
    /// Reason for rejection (empty if accepted).
    pub reject_reason: String,
    /// Host's encoding capabilities.
    pub encoding: EncodingCapabilities,
}

/// Confirmation sent from client to host to finalise the session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConnectionConfirm {
    /// The chosen encoding from the host's capabilities.
    pub chosen_encoding: String,
}

/// Teardown / disconnect message (sent by either side).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Teardown {
    /// Reason for teardown (e.g. "user_disconnect", "timeout", "error").
    pub reason: String,
}

/// Union of all connection management payloads.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ConnectionManagementPayload {
    Request(ConnectionRequest),
    Capabilities(CapabilitiesResponse),
    Confirm(ConnectionConfirm),
    Teardown(Teardown),
}

// ============================================================================
// Screen Frame Chunking (for large frames exceeding one UDP packet)
// ============================================================================

/// A single chunk of a fragmented screen frame.
///
/// Large compressed frames are split into chunks, each sent as a separate
/// `Message` with type `ScreenFrameChunk`. The receiver reassembles chunks
/// by `msg_id`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScreenFrameChunk {
    /// Unique message ID shared by all chunks of the same frame.
    pub msg_id: u32,
    /// Total number of chunks for this frame.
    pub chunk_count: u32,
    /// Index of this chunk (0-based).
    pub chunk_idx: u32,
    /// Total length of the compressed frame data across all chunks.
    pub total_data_len: u32,
    /// Width of the original frame (set only in chunk 0, 0 for others).
    pub width: u32,
    /// Height of the original frame (set only in chunk 0, 0 for others).
    pub height: u32,
    /// This chunk's data (a slice of the compressed frame payload).
    pub data: Vec<u8>,
}

/// Split a compressed frame payload into transmission chunks.
pub fn split_into_chunks(
    payload: &[u8],
    msg_id: u32,
    width: u32,
    height: u32,
) -> Vec<ScreenFrameChunk> {
    let total_data_len = payload.len() as u32;
    let chunk_size = SCREEN_FRAME_CHUNK_DATA_SIZE;
    let chunk_count = ((payload.len() + chunk_size - 1) / chunk_size) as u32;
    let mut chunks = Vec::with_capacity(chunk_count as usize);

    for i in 0..chunk_count as usize {
        let start = i * chunk_size;
        let end = (start + chunk_size).min(payload.len());
        let data = payload[start..end].to_vec();
        chunks.push(ScreenFrameChunk {
            msg_id,
            chunk_count,
            chunk_idx: i as u32,
            total_data_len,
            width: if i == 0 { width } else { 0 },
            height: if i == 0 { height } else { 0 },
            data,
        });
    }

    chunks
}

/// In-progress state for reassembling a fragmented frame from chunks.
#[derive(Debug)]
pub struct FrameAssemblyState {
    chunks: Vec<Option<Vec<u8>>>,
    /// Total number of chunks expected for this frame.
    pub chunk_count: u32,
    total_data_len: u32,
    width: u32,
    height: u32,
    /// Number of chunks received so far.
    pub received_count: u32,
}

impl FrameAssemblyState {
    /// Create a new assembly state expecting `chunk_count` chunks.
    pub fn new(chunk_count: u32, total_data_len: u32, width: u32, height: u32) -> Self {
        let mut chunks = Vec::with_capacity(chunk_count as usize);
        for _ in 0..chunk_count {
            chunks.push(None);
        }
        Self {
            chunks,
            chunk_count,
            total_data_len,
            width,
            height,
            received_count: 0,
        }
    }

    /// Add a received chunk. Returns the complete frame data when all chunks arrive.
    pub fn add_chunk(&mut self, idx: usize, data: Vec<u8>) -> Option<(Vec<u8>, u32, u32)> {
        if idx >= self.chunks.len() || self.chunks[idx].is_some() {
            return None; // duplicate or out-of-range
        }
        self.chunks[idx] = Some(data);
        self.received_count += 1;
        if self.received_count == self.chunk_count {
            let mut full = Vec::with_capacity(self.total_data_len as usize);
            for chunk in &self.chunks {
                if let Some(ref data) = chunk {
                    full.extend_from_slice(data);
                }
            }
            Some((full, self.width, self.height))
        } else {
            None
        }
    }
}

/// Feed a received chunk into the assembly map.
///
/// Returns `Some((complete_data, width, height))` when the last chunk arrives,
/// at which point the assembly state is removed from the map.
pub fn feed_frame_chunk(
    assemblies: &mut std::collections::HashMap<u32, FrameAssemblyState>,
    chunk: ScreenFrameChunk,
) -> Option<(Vec<u8>, u32, u32)> {
    let entry = assemblies.entry(chunk.msg_id).or_insert_with(|| {
        FrameAssemblyState::new(
            chunk.chunk_count,
            chunk.total_data_len,
            chunk.width,
            chunk.height,
        )
    });

    let result = entry.add_chunk(chunk.chunk_idx as usize, chunk.data);
    if result.is_some() {
        assemblies.remove(&chunk.msg_id);
    }
    result
}

// ============================================================================
// Session State
// ============================================================================

/// High-level connection state for both host and client.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConnectionState {
    /// No connection in progress.
    Idle,
    /// Host is listening for incoming connections (host only).
    Listening,
    /// Attempting to establish a connection (client only).
    Connecting,
    /// Session is active.
    Connected,
    /// Requesting disconnection.
    Disconnecting,
    /// Fully disconnected.
    Disconnected,
}

/// Runtime configuration for a session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionConfig {
    /// Host port to connect to (client) or listen on (host).
    pub host_port: u16,
    /// Optional authentication token / PIN.
    pub auth_token: Option<String>,
    /// Preferred encoding method (e.g. "lz4", "h264", "av1").
    pub encoding_preference: Option<String>,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            host_port: DEFAULT_PORT,
            auth_token: None,
            encoding_preference: Some("lz4".to_string()),
        }
    }
}

// ============================================================================
// Utility functions
// ============================================================================

/// Create an ACK message for a given sequence number.
pub fn create_ack(sequence_number: u32) -> Message {
    let ack_payload = sequence_number.to_le_bytes().to_vec();
    Message::new(MessageType::Ack, 0, ack_payload)
}

/// Create a heartbeat message.
pub fn create_heartbeat(sequence_number: u32) -> Message {
    Message::new(MessageType::Heartbeat, sequence_number, Vec::new())
}

/// Create a connection request message.
pub fn create_connection_request(
    sequence_number: u32,
    auth_token: &str,
    protocol_version: u32,
) -> Result<Message, bincode::Error> {
    let payload = ConnectionManagementPayload::Request(ConnectionRequest {
        auth_token: auth_token.to_string(),
        protocol_version,
    });
    let bytes = bincode::serialize(&payload)?;
    Ok(Message::new(MessageType::ConnectionManagement, sequence_number, bytes))
}

/// Create a capabilities response message.
pub fn create_capabilities_response(
    sequence_number: u32,
    accepted: bool,
    reject_reason: &str,
    encoding: EncodingCapabilities,
) -> Result<Message, bincode::Error> {
    let payload = ConnectionManagementPayload::Capabilities(CapabilitiesResponse {
        accepted,
        reject_reason: reject_reason.to_string(),
        encoding,
    });
    let bytes = bincode::serialize(&payload)?;
    Ok(Message::new(MessageType::ConnectionManagement, sequence_number, bytes))
}

/// Create a connection confirm message.
pub fn create_connection_confirm(
    sequence_number: u32,
    chosen_encoding: &str,
) -> Result<Message, bincode::Error> {
    let payload = ConnectionManagementPayload::Confirm(ConnectionConfirm {
        chosen_encoding: chosen_encoding.to_string(),
    });
    let bytes = bincode::serialize(&payload)?;
    Ok(Message::new(MessageType::ConnectionManagement, sequence_number, bytes))
}

/// Create a teardown message.
pub fn create_teardown(
    sequence_number: u32,
    reason: &str,
) -> Result<Message, bincode::Error> {
    let payload = ConnectionManagementPayload::Teardown(Teardown {
        reason: reason.to_string(),
    });
    let bytes = bincode::serialize(&payload)?;
    Ok(Message::new(MessageType::ConnectionManagement, sequence_number, bytes))
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_message_serialization_round_trip() {
        let msg = Message::new(MessageType::Heartbeat, 42, vec![1, 2, 3]);
        let bytes = msg.to_bytes().expect("serialize");
        let decoded = Message::from_bytes(&bytes).expect("deserialize");
        assert_eq!(msg, decoded);
    }

    #[test]
    fn test_message_type_from_u8() {
        assert_eq!(MessageType::from_u8(0x01), Some(MessageType::ControlCommand));
        assert_eq!(MessageType::from_u8(0x02), Some(MessageType::ScreenFrame));
        assert_eq!(MessageType::from_u8(0x03), Some(MessageType::Ack));
        assert_eq!(MessageType::from_u8(0x04), Some(MessageType::Heartbeat));
        assert_eq!(MessageType::from_u8(0x05), Some(MessageType::ConnectionManagement));
        assert_eq!(MessageType::from_u8(0x06), Some(MessageType::ScreenFrameChunk));
        assert_eq!(MessageType::from_u8(0xFF), None);
    }

    #[test]
    fn test_message_header_size() {
        // MessageType(1) + sequence_number(4) + payload_length(4) + reserved(2)
        assert_eq!(Message::header_size(), 11);
    }

    #[test]
    fn test_ack_creation_and_round_trip() {
        let ack = create_ack(12345);
        assert_eq!(ack.message_type, MessageType::Ack);
        let bytes = ack.to_bytes().expect("serialize ack");
        let decoded = Message::from_bytes(&bytes).expect("deserialize ack");
        assert_eq!(ack, decoded);
        // The payload should be the little-endian bytes of 12345
        assert_eq!(ack.payload, 12345u32.to_le_bytes().to_vec());
    }

    #[test]
    fn test_heartbeat_creation_and_round_trip() {
        let hb = create_heartbeat(99);
        assert_eq!(hb.message_type, MessageType::Heartbeat);
        assert_eq!(hb.sequence_number, 99);
        assert!(hb.payload.is_empty());
        let bytes = hb.to_bytes().expect("serialize heartbeat");
        let decoded = Message::from_bytes(&bytes).expect("deserialize heartbeat");
        assert_eq!(hb, decoded);
    }

    #[test]
    fn test_control_command_key_event_round_trip() {
        let key = KeyEvent {
            key_code: 65, // 'A'
            pressed: true,
            modifiers: 0b0010, // Ctrl
            timestamp_us: 1234567890,
        };
        let payload = ControlCommandPayload::Key(key.clone());
        let payload_bytes = bincode::serialize(&payload).expect("serialize key event");
        let msg = Message::new(MessageType::ControlCommand, 1, payload_bytes);
        let bytes = msg.to_bytes().expect("serialize message");
        let decoded = Message::from_bytes(&bytes).expect("deserialize message");
        assert_eq!(msg, decoded);

        // Verify payload deserializes correctly
        let decoded_payload: ControlCommandPayload =
            bincode::deserialize(&decoded.payload).expect("deserialize payload");
        match decoded_payload {
            ControlCommandPayload::Key(k) => {
                assert_eq!(k.key_code, 65);
                assert!(k.pressed);
                assert_eq!(k.modifiers, 0b0010);
                assert_eq!(k.timestamp_us, 1234567890);
            }
            _ => panic!("Expected Key event"),
        }
    }

    #[test]
    fn test_mouse_move_event_round_trip() {
        let mm = MouseMoveEvent {
            dx: 10,
            dy: -5,
            abs_coords: false,
            timestamp_us: 987654321,
        };
        let payload = ControlCommandPayload::MouseMove(mm);
        let payload_bytes = bincode::serialize(&payload).expect("serialize mouse move");
        let msg = Message::new(MessageType::ControlCommand, 2, payload_bytes);
        let bytes = msg.to_bytes().expect("serialize message");
        let decoded = Message::from_bytes(&bytes).expect("deserialize message");
        assert_eq!(msg, decoded);

        let decoded_payload: ControlCommandPayload =
            bincode::deserialize(&decoded.payload).expect("deserialize payload");
        match decoded_payload {
            ControlCommandPayload::MouseMove(m) => {
                assert_eq!(m.dx, 10);
                assert_eq!(m.dy, -5);
                assert!(!m.abs_coords);
            }
            _ => panic!("Expected MouseMove event"),
        }
    }

    #[test]
    fn test_mouse_button_event_round_trip() {
        let mb = MouseButtonEvent {
            button: 0,
            pressed: true,
            x: 1920,
            y: 1080,
            timestamp_us: 111111111,
        };
        let payload = ControlCommandPayload::MouseButton(mb);
        let payload_bytes = bincode::serialize(&payload).expect("serialize mouse button");
        let msg = Message::new(MessageType::ControlCommand, 3, payload_bytes);
        let bytes = msg.to_bytes().expect("serialize message");
        let decoded = Message::from_bytes(&bytes).expect("deserialize message");
        assert_eq!(msg, decoded);

        let decoded_payload: ControlCommandPayload =
            bincode::deserialize(&decoded.payload).expect("deserialize payload");
        match decoded_payload {
            ControlCommandPayload::MouseButton(b) => {
                assert_eq!(b.button, 0);
                assert!(b.pressed);
                assert_eq!(b.x, 1920);
                assert_eq!(b.y, 1080);
            }
            _ => panic!("Expected MouseButton event"),
        }
    }

    #[test]
    fn test_scroll_event_round_trip() {
        let sc = ScrollEvent {
            delta_x: 0,
            delta_y: 120,
            timestamp_us: 222222222,
        };
        let payload = ControlCommandPayload::Scroll(sc);
        let payload_bytes = bincode::serialize(&payload).expect("serialize scroll");
        let msg = Message::new(MessageType::ControlCommand, 4, payload_bytes);
        let bytes = msg.to_bytes().expect("serialize message");
        let decoded = Message::from_bytes(&bytes).expect("deserialize message");
        assert_eq!(msg, decoded);

        let decoded_payload: ControlCommandPayload =
            bincode::deserialize(&decoded.payload).expect("deserialize payload");
        match decoded_payload {
            ControlCommandPayload::Scroll(s) => {
                assert_eq!(s.delta_x, 0);
                assert_eq!(s.delta_y, 120);
            }
            _ => panic!("Expected Scroll event"),
        }
    }

    #[test]
    fn test_connection_management_request_round_trip() {
        let req = ConnectionRequest {
            auth_token: "test-pin-1234".to_string(),
            protocol_version: 1,
        };
        let payload = ConnectionManagementPayload::Request(req);
        let payload_bytes = bincode::serialize(&payload).expect("serialize conn request");
        let msg = Message::new(MessageType::ConnectionManagement, 1, payload_bytes);
        let bytes = msg.to_bytes().expect("serialize message");
        let decoded = Message::from_bytes(&bytes).expect("deserialize message");
        assert_eq!(msg, decoded);

        let decoded_payload: ConnectionManagementPayload =
            bincode::deserialize(&decoded.payload).expect("deserialize payload");
        match decoded_payload {
            ConnectionManagementPayload::Request(r) => {
                assert_eq!(r.auth_token, "test-pin-1234");
                assert_eq!(r.protocol_version, 1);
            }
            _ => panic!("Expected ConnectionRequest"),
        }
    }

    #[test]
    fn test_connection_management_capabilities_round_trip() {
        let caps = CapabilitiesResponse {
            accepted: true,
            reject_reason: String::new(),
            encoding: EncodingCapabilities {
                lz4_delta: true,
                h264_low_delay: true,
                av1_rt: false,
                max_width: 3840,
                max_height: 2160,
            },
        };
        let payload = ConnectionManagementPayload::Capabilities(caps);
        let payload_bytes = bincode::serialize(&payload).expect("serialize caps");
        let msg = Message::new(MessageType::ConnectionManagement, 2, payload_bytes);
        let bytes = msg.to_bytes().expect("serialize message");
        let decoded = Message::from_bytes(&bytes).expect("deserialize message");
        assert_eq!(msg, decoded);

        let decoded_payload: ConnectionManagementPayload =
            bincode::deserialize(&decoded.payload).expect("deserialize payload");
        match decoded_payload {
            ConnectionManagementPayload::Capabilities(c) => {
                assert!(c.accepted);
                assert!(c.encoding.lz4_delta);
                assert!(c.encoding.h264_low_delay);
                assert!(!c.encoding.av1_rt);
                assert_eq!(c.encoding.max_width, 3840);
                assert_eq!(c.encoding.max_height, 2160);
            }
            _ => panic!("Expected CapabilitiesResponse"),
        }
    }

    #[test]
    fn test_connection_management_confirm_round_trip() {
        let confirm = ConnectionConfirm {
            chosen_encoding: "lz4".to_string(),
        };
        let payload = ConnectionManagementPayload::Confirm(confirm);
        let payload_bytes = bincode::serialize(&payload).expect("serialize confirm");
        let msg = Message::new(MessageType::ConnectionManagement, 3, payload_bytes);
        let bytes = msg.to_bytes().expect("serialize message");
        let decoded = Message::from_bytes(&bytes).expect("deserialize message");
        assert_eq!(msg, decoded);

        let decoded_payload: ConnectionManagementPayload =
            bincode::deserialize(&decoded.payload).expect("deserialize payload");
        match decoded_payload {
            ConnectionManagementPayload::Confirm(c) => {
                assert_eq!(c.chosen_encoding, "lz4");
            }
            _ => panic!("Expected ConnectionConfirm"),
        }
    }

    #[test]
    fn test_teardown_round_trip() {
        let td = Teardown {
            reason: "user_disconnect".to_string(),
        };
        let payload = ConnectionManagementPayload::Teardown(td);
        let payload_bytes = bincode::serialize(&payload).expect("serialize teardown");
        let msg = Message::new(MessageType::ConnectionManagement, 4, payload_bytes);
        let bytes = msg.to_bytes().expect("serialize message");
        let decoded = Message::from_bytes(&bytes).expect("deserialize message");
        assert_eq!(msg, decoded);

        let decoded_payload: ConnectionManagementPayload =
            bincode::deserialize(&decoded.payload).expect("deserialize payload");
        match decoded_payload {
            ConnectionManagementPayload::Teardown(t) => {
                assert_eq!(t.reason, "user_disconnect");
            }
            _ => panic!("Expected Teardown"),
        }
    }

    #[test]
    fn test_session_config_default() {
        let config = SessionConfig::default();
        assert_eq!(config.host_port, DEFAULT_PORT);
        assert_eq!(config.auth_token, None);
        assert_eq!(config.encoding_preference, Some("lz4".to_string()));
    }

    #[test]
    fn test_connection_state_variants() {
        assert_eq!(ConnectionState::Idle as u8, 0);
        assert_eq!(ConnectionState::Listening as u8, 1);
        assert_eq!(ConnectionState::Connecting as u8, 2);
        assert_eq!(ConnectionState::Connected as u8, 3);
        assert_eq!(ConnectionState::Disconnecting as u8, 4);
        assert_eq!(ConnectionState::Disconnected as u8, 5);
    }

    #[test]
    fn test_utility_functions() {
        // Test create_ack
        let ack = create_ack(42);
        assert_eq!(ack.message_type, MessageType::Ack);
        assert_eq!(ack.payload.len(), 4);

        // Test create_heartbeat
        let hb = create_heartbeat(1);
        assert_eq!(hb.message_type, MessageType::Heartbeat);
        assert_eq!(hb.payload.len(), 0);

        // Test create_connection_request
        let req = create_connection_request(1, "mypin", 1).expect("create connection request");
        assert_eq!(req.message_type, MessageType::ConnectionManagement);

        // Test create_teardown
        let td = create_teardown(2, "bye").expect("create teardown");
        assert_eq!(td.message_type, MessageType::ConnectionManagement);
    }
}
