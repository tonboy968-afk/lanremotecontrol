//! Network module for the LANRemoteControl client application.
//!
//! Provides a UDP client and a handshake helper for connecting to the host.

use std::collections::HashMap;
use std::io;
use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use lanremotecontrol_common::*;
use lanremotecontrol_common::encoding;

// ============================================================================
// UdpClient
// ============================================================================

/// A connected UDP socket used to communicate with the host.
///
/// Internally wraps the socket in `Arc` so that multiple threads can share
/// the same client (e.g. one for sending, another for receiving).
pub struct UdpClient {
    socket: Arc<UdpSocket>,
}

impl UdpClient {
    /// Resolve `host:port` and create a connected UDP socket.
    ///
    /// The socket is bound to an ephemeral local port and connected to the
    /// resolved host address so that `send` and `recv` can be used without
    /// supplying an address each time.
    pub fn connect(host: &str, port: u16) -> io::Result<Self> {
        let addr_str = format!("{}:{}", host, port);
        let addr = addr_str
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| io::Error::new(io::ErrorKind::AddrNotAvailable, "could not resolve address"))?;

        let local_addr = if addr.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" };
        let socket = UdpSocket::bind(local_addr)?;
        socket.connect(addr)?;

        // Increase UDP buffer sizes to handle burst of frame chunks
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawSocket;
            const SOL_SOCKET: i32 = 0xFFFF;
            const SO_RCVBUF: i32 = 0x1002;
            const SO_SNDBUF: i32 = 0x1001;
            let buf_size: i32 = 4 * 1024 * 1024; // 4MB
            unsafe {
                let raw = socket.as_raw_socket() as usize;
                #[link(name = "ws2_32")]
                extern "system" {
                    fn setsockopt(s: usize, level: i32, optname: i32, optval: *const i32, optlen: i32) -> i32;
                }
                setsockopt(raw, SOL_SOCKET, SO_RCVBUF, &buf_size, 4);
                setsockopt(raw, SOL_SOCKET, SO_SNDBUF, &buf_size, 4);
            }
        }

        Ok(Self { socket: Arc::new(socket) })
    }

    /// Serialise and send a message to the connected host.
    pub fn send(&self, msg: &Message) -> io::Result<()> {
        let bytes = msg
            .to_bytes()
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        self.socket.send(&bytes)?;
        Ok(())
    }

    /// Receive a message with the given timeout in milliseconds.
    ///
    /// Sets `set_read_timeout` before each call, so be aware that this
    /// affects subsequent operations on the same socket.
    pub fn receive(&self, timeout_ms: u64) -> io::Result<Message> {
        self.socket
            .set_read_timeout(Some(Duration::from_millis(timeout_ms)))?;
        let mut buf = [0u8; MAX_PACKET_SIZE + 64];
        let len = self.socket.recv(&mut buf)?;
        Message::from_bytes(&buf[..len])
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }

    /// Local bound address.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.socket.local_addr()
    }

    /// Peer (connected host) address.
    #[allow(dead_code)]
    pub fn peer_addr(&self) -> io::Result<SocketAddr> {
        self.socket.peer_addr()
    }
}

// ============================================================================
// HandshakeError
// ============================================================================

/// Errors that can occur during the handshake with the host.
#[derive(Debug)]
pub enum HandshakeError {
    /// I/O error (e.g. connection failure).
    Io(io::Error),
    /// Reply was not received within the expected time.
    Timeout,
    /// Host rejected the connection attempt.
    Rejected(String),
    /// Received a message with an unexpected type.
    WrongMessageType,
    /// Serialisation or deserialisation failure.
    Serialization(String),
}

impl std::fmt::Display for HandshakeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "IO error: {}", e),
            Self::Timeout => write!(f, "handshake timed out"),
            Self::Rejected(r) => write!(f, "connection rejected: {}", r),
            Self::WrongMessageType => write!(f, "unexpected message type from host"),
            Self::Serialization(e) => write!(f, "serialization error: {}", e),
        }
    }
}

impl std::error::Error for HandshakeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for HandshakeError {
    fn from(e: io::Error) -> Self {
        if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::TimedOut {
            Self::Timeout
        } else {
            Self::Io(e)
        }
    }
}

// ============================================================================
// HandshakeClient
// ============================================================================

/// Implements the connection handshake (request → capabilities → confirm)
/// on the client side.
pub struct HandshakeClient;

impl HandshakeClient {
    /// Perform the full three-way handshake with the host.
    ///
    /// 1. Send a `ConnectionRequest`.
    /// 2. Receive the host's `CapabilitiesResponse`.
    /// 3. Send a `ConnectionConfirm`.
    ///
    /// Returns the host's capabilities on success.
    pub fn perform_handshake(
        client: &UdpClient,
        auth_token: &str,
        seq: u32,
    ) -> Result<CapabilitiesResponse, HandshakeError> {
        // Step 1: Send connection request
        let req_msg = create_connection_request(seq, auth_token, 1)
            .map_err(|e| HandshakeError::Serialization(e.to_string()))?;
        client.send(&req_msg)?;

        // Step 2: Receive capabilities response (with generous timeout)
        let response_msg = client.receive(ACK_TIMEOUT_MS * 6)?; // 300 ms

        if response_msg.message_type != MessageType::ConnectionManagement {
            return Err(HandshakeError::WrongMessageType);
        }

        let payload: ConnectionManagementPayload = bincode::deserialize(&response_msg.payload)
            .map_err(|e| HandshakeError::Serialization(e.to_string()))?;

        match payload {
            ConnectionManagementPayload::Capabilities(caps) => {
                if !caps.accepted {
                    return Err(HandshakeError::Rejected(caps.reject_reason));
                }

                // Step 3: Send connection confirm
                // Pick the best encoding from what the host supports
                let chosen = if caps.encoding.lz4_delta {
                    "lz4"
                } else if caps.encoding.h264_low_delay {
                    "h264"
                } else if caps.encoding.av1_rt {
                    "av1"
                } else {
                    "none"
                };
                let confirm_msg = create_connection_confirm(seq.wrapping_add(1), chosen)
                    .map_err(|e| HandshakeError::Serialization(e.to_string()))?;
                client.send(&confirm_msg)?;

                Ok(caps)
            }
            _ => Err(HandshakeError::WrongMessageType),
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::UdpSocket as RawUdpSocket;

    /// Helper: create a raw UDP socket bound to localhost:0, send/receive
    /// messages using the protocol helpers.  This test verifies that the
    /// client can connect and communicate with a minimal host server.
    #[test]
    fn test_udp_client_connect_send_receive() {
        // Create a host-style raw socket that acts as the "host server"
        let host = RawUdpSocket::bind("127.0.0.1:0").expect("host bind");
        host.set_read_timeout(Some(Duration::from_millis(200)))
            .expect("host set timeout");
        let host_addr = host.local_addr().expect("host addr");

        // Create client connected to this host
        let client =
            UdpClient::connect("127.0.0.1", host_addr.port()).expect("client connect");

        // Send a message from client to host
        let msg = Message::new(MessageType::Heartbeat, 1, vec![10, 20, 30]);
        client.send(&msg).expect("client send");

        // Receive on raw host socket
        let mut buf = [0u8; MAX_PACKET_SIZE + 64];
        let (len, _src) = host.recv_from(&mut buf).expect("host recv");
        let received = Message::from_bytes(&buf[..len]).expect("deserialize");
        assert_eq!(received, msg);
    }

    #[test]
    fn test_udp_client_bidirectional() {
        // Host raw socket
        let host = RawUdpSocket::bind("127.0.0.1:0").expect("host bind");
        host.set_read_timeout(Some(Duration::from_millis(200)))
            .expect("host set timeout");
        let host_addr = host.local_addr().expect("host addr");

        // Client
        let client =
            UdpClient::connect("127.0.0.1", host_addr.port()).expect("client connect");
        let client_local = client.local_addr().expect("client local addr");

        // Client → Host
        let c2h = Message::new(MessageType::Ack, 7, vec![]);
        client.send(&c2h).expect("client send");

        let mut buf = [0u8; MAX_PACKET_SIZE + 64];
        let (len, src) = host.recv_from(&mut buf).expect("host recv");
        let rcvd = Message::from_bytes(&buf[..len]).expect("deserialize");
        assert_eq!(rcvd, c2h);
        assert_eq!(src, client_local);

        // Host → Client
        let h2c = Message::new(MessageType::ScreenFrame, 99, vec![0xAB; 128]);
        let bytes = h2c.to_bytes().expect("serialize");
        host.send_to(&bytes, src).expect("host send_to");

        let rcvd = client.receive(200).expect("client receive");
        assert_eq!(rcvd, h2c);
    }
}

// ============================================================================
// Frame Receiver (background thread)
// ============================================================================

/// Shared frame buffer type: `(raw_BGRA_data, width, height)`.
pub type FrameBuffer = Arc<Mutex<Option<(Vec<u8>, u32, u32)>>>;

/// Debug log writer (append-only text file in the working directory).
/// Helps diagnose frame pipeline issues when the GUI window has no console.
fn debug_log(msg: impl AsRef<str>) {
    use std::io::Write;
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let log_path = std::env::var("APPDATA")
        .map(|p| format!(r"{}\lanremotecontrol\lrc_client_debug.log", p))
        .unwrap_or_else(|_| "lrc_client_debug.log".to_string());
    if let Some(parent) = std::path::Path::new(&log_path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        let _ = writeln!(f, "[{}] {}", now_ms, msg.as_ref());
    }
    eprintln!("[LRC] {}", msg.as_ref());
}

/// Request an immediate full keyframe from the host (decoder refresh / DCC).
///
/// Rate-limited to once per 500ms to avoid flooding the host when many
/// frames are lost in a burst (e.g. fast motion over lossy Wi-Fi).
fn request_keyframe(client: &UdpClient, last_req: &mut Instant) {
    if last_req.elapsed() > Duration::from_millis(500) {
        let msg = Message::new(MessageType::RequestKeyframe, 0, Vec::new());
        if client.send(&msg).is_ok() {
            *last_req = Instant::now();
            debug_log("Requested keyframe (decoder refresh) from host");
        }
    }
}

/// Background loop that receives fragmented screen frames, reassembles them
/// and stores the decompressed BGRA frame in `frame_buffer`.
///
/// Handles both full frames (`ScreenFrameChunk`) and delta frames
/// (`ScreenFrameChunkDelta`).  Delta frames are decompressed into a list of
/// `DeltaRegion`s and applied onto a persistent BGRA buffer.
///
/// Runs until the socket read returns a permanent error.
pub fn run_frame_receiver(client: Arc<UdpClient>, frame_buffer: FrameBuffer) {
    let mut assemblies: HashMap<u32, (FrameAssemblyState, MessageType)> = HashMap::new();
    let mut stale_cleanup = Instant::now();

    // Persistent BGRA buffer for delta frame composition.
    // Initialised on the first full frame.
    let mut persistent_bgra: Option<(Vec<u8>, u32, u32)> = None;

    // Last successfully-applied frame id. Used to detect dropped frames
    // (a gap in msg_id sequence means a frame was lost on the wire).
    let mut last_applied_msg_id: u32 = 0;
    // Rate-limit timestamp for keyframe requests.
    let mut last_kf_req = Instant::now() - Duration::from_secs(10);

    loop {
        match client.receive(100) {
            Ok(msg) => {
                match msg.message_type {
                    MessageType::ScreenFrameChunk | MessageType::ScreenFrameChunkDelta => {
                        let is_delta = msg.message_type == MessageType::ScreenFrameChunkDelta;
                        if let Ok(chunk) = bincode::deserialize::<ScreenFrameChunk>(&msg.payload) {
                            let msg_id = chunk.msg_id;
                            // Log first chunk of each frame to track progress
                            if chunk.chunk_idx == 0 {
                                // Gap detection: if this frame is more than 1 ahead of the
                                // last applied frame, a frame was dropped on the wire →
                                // request a keyframe so the decoder can re-sync.
                                if last_applied_msg_id != 0 && msg_id > last_applied_msg_id + 1 {
                                    request_keyframe(&client, &mut last_kf_req);
                                }
                                debug_log(format!(
                                    "Frame chunk start: msg_id={}, chunks={}, size={}, type={}",
                                    chunk.msg_id, chunk.chunk_count, chunk.total_data_len,
                                    if is_delta { "delta" } else { "full" },
                                ));
                            }
                            if let Some((full_data, w, h)) =
                                feed_frame_chunk_typed(&mut assemblies, chunk, msg.message_type)
                            {
                                debug_log(format!(
                                    "Frame assembled: {}x{} ({} bytes), type={}, decoding…",
                                    w, h, full_data.len(),
                                    if is_delta { "delta" } else { "full" },
                                ));

                                if is_delta {
                                    // Delta frame: decompress into regions and apply
                                    match encoding::decompress_delta(&full_data) {
                                        Ok(regions) => {
                                            debug_log(format!(
                                                "Delta decompress OK: {} regions",
                                                regions.len()
                                            ));
                                            // Apply delta regions to persistent buffer
                                            if let Some((ref mut bgra, bw, bh)) = persistent_bgra {
                                                for region in &regions {
                                                    // Apply each region's pixel data to the buffer
                                                    let region_stride = region.width * 4;
                                                    for y in 0..region.height {
                                                        let dst_y = region.y + y;
                                                        if dst_y >= bh {
                                                            break;
                                                        }
                                                        let dst_x_start = region.x * 4;
                                                        let src_start = (y * region_stride) as usize;
                                                        let src_end = src_start + (region.width * 4) as usize;
                                                        let dst_start = ((dst_y * bw * 4) + dst_x_start) as usize;
                                                        let dst_end = dst_start + (region.width * 4) as usize;
                                                        // Clamp to buffer bounds
                                                        let copy_len = src_end - src_start;
                                                        let dst_clamped_end = dst_start + copy_len;
                                                        if dst_clamped_end <= bgra.len() && src_end <= region.lz4_compressed_data.len() {
                                                            bgra[dst_start..dst_clamped_end]
                                                                .copy_from_slice(
                                                                    &region.lz4_compressed_data[src_start..src_end],
                                                                );
                                                        }
                                                    }
                                                }
                                                // Push updated buffer to frame_buffer
                                                debug_log(format!(
                                                    "Delta applied: {} regions -> {}x{} buffer",
                                                    regions.len(), bw, bh
                                                ));
                                                if let Ok(mut guard) = frame_buffer.lock() {
                                                    *guard = Some((bgra.clone(), bw, bh));
                                                }
                                                last_applied_msg_id = msg_id;
                                            } else {
                                                debug_log("Delta frame received but no persistent buffer — skipping".to_string());
                                            }
                                        }
                                        Err(e) => {
                                            debug_log(format!(
                                                "Delta decompress FAILED: {} (data={} bytes)",
                                                e, full_data.len()
                                            ));
                                            // Lost/corrupt delta frame → ask for a keyframe
                                            request_keyframe(&client, &mut last_kf_req);
                                        }
                                    }
                                } else {
                                    // Full frame: LZ4 decompress directly
                                    let expected_size = (w * h * 4) as usize;
                                    match encoding::decompress_full_frame(&full_data, expected_size) {
                                        Ok(bgra) => {
                                            debug_log(format!(
                                                "LZ4 full-frame decompress OK: {}×{} -> {} bytes",
                                                w, h, bgra.len()
                                            ));
                                            // Update persistent buffer for future delta frames
                                            persistent_bgra = Some((bgra.clone(), w, h));
                                            if let Ok(mut guard) = frame_buffer.lock() {
                                                *guard = Some((bgra, w, h));
                                            }
                                            last_applied_msg_id = msg_id;
                                        }
                                        Err(e) => {
                                            debug_log(format!(
                                                "LZ4 full-frame decompress FAILED: {} (data={} bytes, {}x{})",
                                                e, full_data.len(), w, h
                                            ));
                                            // Keyframe itself was lost/corrupt → re-request so
                                            // the decoder can still re-sync.
                                            request_keyframe(&client, &mut last_kf_req);
                                        }
                                    }
                                }
                            }
                        }
                    }
                    MessageType::Heartbeat => {
                        // 收到主机心跳 → 回复 ACK
                        let ack = create_ack(msg.sequence_number);
                        let _ = client.send(&ack);
                    }
                    _ => {
                        // 其他消息类型暂时忽略
                    }
                }
            }
            Err(ref e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                // Normal timeout — continue
            }
            Err(e) => {
                eprintln!("[!] Frame receiver error: {}, exiting", e);
                break;
            }
        }

        // Periodically clean stale partial assemblies (incomplete frames stuck in map)
        if stale_cleanup.elapsed() > Duration::from_secs(2) {
            let before = assemblies.len();
            assemblies.retain(|_, (state, _)| {
                // Keep only assemblies that are still in-progress
                state.received_count > 0 && state.received_count < state.chunk_count
            });
            let removed = before - assemblies.len();
            if removed > 0 {
                debug_log(format!("Stale cleanup: removed {} incomplete assemblies", removed));
                // Incomplete frames stuck in the map = dropped chunks on the wire.
                // Request a keyframe to re-sync the decoder.
                request_keyframe(&client, &mut last_kf_req);
            }
            stale_cleanup = Instant::now();
        }
    }
}

/// Feed a received chunk into the assembly map, tracking message type.
///
/// Returns `Some((complete_data, width, height))` when the last chunk arrives,
/// at which point the assembly state is removed from the map.
pub fn feed_frame_chunk_typed(
    assemblies: &mut std::collections::HashMap<u32, (FrameAssemblyState, MessageType)>,
    chunk: ScreenFrameChunk,
    msg_type: MessageType,
) -> Option<(Vec<u8>, u32, u32)> {
    let entry = assemblies.entry(chunk.msg_id).or_insert_with(|| {
        (
            FrameAssemblyState::new(
                chunk.chunk_count,
                chunk.total_data_len,
                chunk.width,
                chunk.height,
            ),
            msg_type,
        )
    });

    let result = entry.0.add_chunk(chunk.chunk_idx as usize, chunk.data);
    if result.is_some() {
        assemblies.remove(&chunk.msg_id);
    }
    result
}

/// 将像素坐标归一化为 Windows 绝对坐标 (0..65535)
///
/// Windows `MOUSEEVENTF_ABSOLUTE` 要求 `dx`/`dy` 的范围为 0..65535。
/// 此函数将屏幕像素坐标映射到该范围。
pub fn normalize_abs_coord(pixel: f32, screen_size: u32) -> i32 {
    if screen_size == 0 {
        return 0;
    }
    let normalized = (pixel / screen_size as f32) * 65535.0;
    (normalized.round() as i32).clamp(0, 65535)
}

/// Convenience function: convert BGRA pixel data to RGBA for egui.
pub fn bgra_to_rgba(bgra: &[u8]) -> Vec<u8> {
    let mut rgba = Vec::with_capacity(bgra.len());
    for chunk in bgra.chunks_exact(4) {
        rgba.push(chunk[2]); // R
        rgba.push(chunk[1]); // G
        rgba.push(chunk[0]); // B
        rgba.push(chunk[3]); // A (unchanged)
    }
    rgba
}
