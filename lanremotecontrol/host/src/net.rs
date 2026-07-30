//! Network module for the LANRemoteControl host service.
//!
//! Provides UDP listener, connection management, and heartbeat functionality.

use std::collections::{HashMap, HashSet};
use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use lanremotecontrol_common::*;

// ============================================================================
// UdpListener
// ============================================================================

/// UDP listener that receives and sends protocol messages.
pub struct UdpListener {
    socket: UdpSocket,
}

impl UdpListener {
    /// Bind to a port, enable SO_REUSEADDR, set TOS for low delay.
    ///
    /// If TOS cannot be set (e.g. on some platforms), a warning is printed
    /// to stderr and the socket continues without it.
    pub fn bind(port: u16) -> io::Result<Self> {
        let socket = UdpSocket::bind(format!("0.0.0.0:{}", port))?;
        socket.set_read_timeout(Some(Duration::from_millis(1)))?;

        // Increase UDP receive buffer to handle burst of frame chunks
        // Default Windows UDP buffer is 8KB — far too small for 400+ chunks
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

        // Enable SO_REUSEADDR for quick restarts (Unix-only; best-effort)
        #[cfg(unix)]
        if let Err(e) = socket.set_reuseaddr(true) {
            eprintln!("Warning: could not set SO_REUSEADDR: {}", e);
        }

        // Configure TOS for low-delay traffic (0xB8 = DSCP AF43)
        // This is best-effort; some platforms may not support it
        #[cfg(unix)]
        if let Err(e) = socket.set_tos(0xB8) {
            eprintln!("Warning: could not set TOS on socket: {}", e);
        }

        Ok(Self { socket })
    }

    /// Block and receive a single message. Returns the parsed `Message` and
    /// the sender's socket address.
    pub fn receive_message(&self) -> io::Result<(Message, SocketAddr)> {
        let mut buf = [0u8; MAX_PACKET_SIZE + 64];
        let (len, addr) = self.socket.recv_from(&mut buf)?;
        let msg = Message::from_bytes(&buf[..len])
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        Ok((msg, addr))
    }

    /// Send a serialised message to `dest`.
    pub fn send_message(&self, msg: &Message, dest: SocketAddr) -> io::Result<()> {
        let bytes = msg
            .to_bytes()
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        self.socket.send_to(&bytes, dest)?;
        Ok(())
    }

    /// Get the local bound address.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.socket.local_addr()
    }

    /// Send a large payload as fragmented `ScreenFrameChunk` messages.
    ///
    /// The payload is split into `SCREEN_FRAME_CHUNK_DATA_SIZE`-sized chunks,
    /// each wrapped in a `ScreenFrameChunk` message and sent as a separate
    /// UDP datagram.  Chunks are sent exactly once — on a healthy LAN, packet
    /// loss is negligible and the bandwidth savings (≈50%) reduce burst
    /// congestion, which is the primary cause of packet drops at high frame
    /// rates.
    ///
    /// `chunk_type` should be `ScreenFrameChunk` for full frames or
    /// `ScreenFrameChunkDelta` for delta frames.
    pub fn send_fragmented(
        &self,
        msg_id: u32,
        payload: &[u8],
        seq: u32,
        dest: SocketAddr,
        width: u32,
        height: u32,
        chunk_type: MessageType,
    ) -> io::Result<()> {
        let chunks = split_into_chunks(payload, msg_id, width, height);
        // Pre-serialise all chunk messages once.
        for (i, chunk) in chunks.iter().enumerate() {
            let chunk_bytes = bincode::serialize(chunk)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            let msg = Message::new(chunk_type, seq, chunk_bytes);
            let wire = msg
                .to_bytes()
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            self.socket.send_to(&wire, dest)?;
            // Yield every 64 chunks to avoid saturating the NIC/OS buffer
            // on large full frames (400+ chunks)
            if i > 0 && i % 64 == 0 {
                std::thread::yield_now();
            }
        }
        Ok(())
    }
}

// ============================================================================
// PendingConnection & PendingState
// ============================================================================

/// State of a pending connection during the handshake.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingState {
    /// Waiting for the client to send a `ConnectionConfirm`.
    AwaitingConfirm,
    /// Handshake completed; the connection is considered active.
    Connected,
}

/// Tracks a connection attempt that is in the middle of the handshake.
#[derive(Debug, Clone)]
pub struct PendingConnection {
    /// Timestamp when the connection request was received.
    #[allow(dead_code)]
    pub created_at: Instant,
    /// Current state within the handshake.
    pub state: PendingState,
}

// ============================================================================
// ConnectionManager
// ============================================================================

/// Manages the lifecycle of incoming connections.
///
/// Maintains a set of pending (handshake-in-progress) connections and a
/// set of fully-established connections.
pub struct ConnectionManager {
    pending_connections: HashMap<SocketAddr, PendingConnection>,
    active_connections: HashSet<SocketAddr>,
    seq_counter: u32,
}

impl ConnectionManager {
    /// Create a new empty manager.
    pub fn new() -> Self {
        Self {
            pending_connections: HashMap::new(),
            active_connections: HashSet::new(),
            seq_counter: 1,
        }
    }

    /// Process an incoming `Message` from `addr`.
    ///
    /// If the message triggers a reply, returns `Ok(Some(msg))`; otherwise
    /// returns `Ok(None)`.
    pub fn handle_message(
        &mut self,
        msg: Message,
        addr: SocketAddr,
    ) -> io::Result<Option<Message>> {
        match msg.message_type {
            MessageType::ConnectionManagement => {
                let payload: ConnectionManagementPayload =
                    bincode::deserialize(&msg.payload)
                        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

                match payload {
                    ConnectionManagementPayload::Request(_req) => {
                        // 清除该地址可能残留的旧连接（客户端重启后端口可能相同）
                        self.handle_disconnect(addr);
                        // Client wants to connect → respond with capabilities
                        let seq = self.next_seq();
                        let encoding = EncodingCapabilities {
                            lz4_delta: true,
                            h264_low_delay: true,
                            av1_rt: false,
                            max_width: 3840,
                            max_height: 2160,
                        };
                        self.pending_connections.insert(
                            addr,
                            PendingConnection {
                                created_at: Instant::now(),
                                state: PendingState::AwaitingConfirm,
                            },
                        );
                        let reply = create_capabilities_response(seq, true, "", encoding)
                            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
                        Ok(Some(reply))
                    }
                    ConnectionManagementPayload::Confirm(_confirm) => {
                        // Client confirmed the session → activate
                        if let Some(pending) = self.pending_connections.get_mut(&addr) {
                            pending.state = PendingState::Connected;
                        }
                        self.active_connections.insert(addr);
                        self.pending_connections.remove(&addr);
                        Ok(None)
                    }
                    ConnectionManagementPayload::Teardown(_) => {
                        self.handle_disconnect(addr);
                        Ok(None)
                    }
                    ConnectionManagementPayload::Capabilities(_) => {
                        // A host should not receive a Capabilities message from
                        // a client during normal protocol flow.
                        Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "unexpected Capabilities message from client",
                        ))
                    }
                }
            }
            _ => {
                // Non-connection-management messages are passed through
                Ok(None)
            }
        }
    }

    /// Remove a connection (pending or active) by address.
    pub fn handle_disconnect(&mut self, addr: SocketAddr) {
        self.pending_connections.remove(&addr);
        self.active_connections.remove(&addr);
    }

    /// Check whether `addr` has a fully established connection.
    #[allow(dead_code)]
    pub fn is_connected(&self, addr: &SocketAddr) -> bool {
        self.active_connections.contains(addr)
    }

    /// Return the number of active (fully established) connections.
    pub fn active_count(&self) -> usize {
        self.active_connections.len()
    }

    /// Iterate over all active connection addresses.
    pub fn active_addrs(&self) -> Vec<SocketAddr> {
        self.active_connections.iter().copied().collect()
    }

    fn next_seq(&mut self) -> u32 {
        let seq = self.seq_counter;
        self.seq_counter = self.seq_counter.wrapping_add(1);
        seq
    }
}

// ============================================================================
// HeartbeatManager
// ============================================================================

/// Tracks heartbeat state for a single connection and determines whether
/// the peer is still alive.
pub struct HeartbeatManager {
    interval: Duration,
    last_tick: Instant,
    /// Sequence number for the *next* heartbeat to send.
    send_seq: u32,
    /// Most recent heartbeat seq for which we received an ACK.
    last_acked_seq: Option<u32>,
    /// Maximum number of consecutive missed heartbeats before declaring
    /// the peer dead.
    max_missed: u32,
}

impl HeartbeatManager {
    /// Create a new manager that sends heartbeats every `interval`.
    pub fn new(interval: Duration) -> Self {
        Self {
            interval,
            last_tick: Instant::now(),
            send_seq: 0,
            last_acked_seq: None,
            max_missed: 3,
        }
    }

    /// Must be called periodically.  Returns `true` when it is time to send
    /// a heartbeat (i.e. the interval has elapsed).
    ///
    /// Each time `true` is returned, the internal sequence counter is
    /// advanced so that the caller can tag the outgoing heartbeat.
    pub fn tick(&mut self) -> bool {
        let now = Instant::now();
        if now.duration_since(self.last_tick) >= self.interval {
            self.last_tick = now;
            self.send_seq = self.send_seq.wrapping_add(1);
            true
        } else {
            false
        }
    }

    /// Register that an ACK for heartbeat `seq` was received.
    pub fn received_ack(&mut self, seq: u32) {
        match self.last_acked_seq {
            Some(last) => {
                // Only advance if `seq` is more recent than `last`
                // (handle wrapping with wrapping_sub comparison)
                let diff = seq.wrapping_sub(last);
                if diff > 0 && diff <= u32::MAX / 2 {
                    self.last_acked_seq = Some(seq);
                }
            }
            None => {
                self.last_acked_seq = Some(seq);
            }
        }
    }

    /// Returns `false` when more than `max_missed` consecutive heartbeats
    /// have been sent without receiving an ACK.
    pub fn check_alive(&self) -> bool {
        let missed = match self.last_acked_seq {
            Some(last_acked) => self.send_seq.wrapping_sub(last_acked),
            None => self.send_seq,
        };
        // At threshold (== max_missed) we are still considered alive;
        // only when strictly exceeded do we declare the peer dead.
        missed <= self.max_missed
    }

    /// Current heartbeat sequence number (the one to use for the next
    /// outgoing heartbeat).
    pub fn current_seq(&self) -> u32 {
        self.send_seq
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_udp_listener_bind() {
        let listener = UdpListener::bind(0).expect("bind on port 0");
        let addr = listener.local_addr().expect("local_addr");
        assert_ne!(addr.port(), 0, "port should be assigned");
    }

    #[test]
    fn test_udp_listener_send_receive() {
        // Create raw UDP sockets bound to 127.0.0.1 so that send_to works
        // on all platforms (binding to 0.0.0.0 yields 0.0.0.0:port which
        // is not routable on Windows).
        use std::net::UdpSocket as RawUdpSocket;

        let raw_a = RawUdpSocket::bind("127.0.0.1:0").expect("bind raw A");
        let raw_b = RawUdpSocket::bind("127.0.0.1:0").expect("bind raw B");
        let addr_b = raw_b.local_addr().expect("addr B");

        // Manually construct UdpListener wrappers (they share the socket)
        let a = UdpListener { socket: raw_a };
        // We don't need a full UdpListener for B; just use raw_b wrapped
        let b = UdpListener { socket: raw_b };

        let msg = Message::new(MessageType::Heartbeat, 42, vec![1, 2, 3]);
        a.send_message(&msg, addr_b).expect("send from A to B");

        let (received, _addr) = b.receive_message().expect("receive on B");
        assert_eq!(received, msg);
    }

    #[test]
    fn test_connection_manager_handshake() {
        let mut mgr = ConnectionManager::new();
        let client_addr: SocketAddr = "127.0.0.1:12345".parse().unwrap();

        // --- Simulate connection request ---
        let req_msg = create_connection_request(1, "test-pin", 1).expect("create request");
        let response = mgr
            .handle_message(req_msg, client_addr)
            .expect("handle request")
            .expect("should have a response");

        assert_eq!(
            response.message_type,
            MessageType::ConnectionManagement
        );
        let caps: ConnectionManagementPayload =
            bincode::deserialize(&response.payload).expect("deserialize caps");
        match caps {
            ConnectionManagementPayload::Capabilities(c) => {
                assert!(c.accepted, "should be accepted");
                assert!(c.encoding.lz4_delta, "should support LZ4");
            }
            _ => panic!("expected Capabilities response"),
        }

        // --- Simulate confirm ---
        let confirm_msg =
            create_connection_confirm(2, "lz4").expect("create confirm");
        let maybe = mgr
            .handle_message(confirm_msg, client_addr)
            .expect("handle confirm");
        assert!(maybe.is_none(), "confirm should not produce a response");

        assert!(mgr.is_connected(&client_addr));
        assert_eq!(mgr.active_count(), 1);

        // --- Simulate teardown ---
        let teardown_msg = create_teardown(3, "bye").expect("create teardown");
        mgr.handle_message(teardown_msg, client_addr)
            .expect("handle teardown");

        assert!(!mgr.is_connected(&client_addr));
        assert_eq!(mgr.active_count(), 0);
    }

    #[test]
    fn test_connection_manager_rejected() {
        // We don't have a reject-message flow in the current protocol,
        // but at least verify the manager handles it without panicking.
        let mut mgr = ConnectionManager::new();
        let addr: SocketAddr = "127.0.0.1:54321".parse().unwrap();
        mgr.handle_disconnect(addr);
        assert!(!mgr.is_connected(&addr));
    }

    #[test]
    fn test_heartbeat_tick_and_ack() {
        let mut hbm = HeartbeatManager::new(Duration::from_secs(1));

        fn force_tick(hbm: &mut HeartbeatManager) {
            hbm.last_tick = Instant::now()
                .checked_sub(Duration::from_secs(60))
                .unwrap();
            assert!(hbm.tick(), "tick should fire after resetting last_tick");
        }

        // First tick → advances seq to 1
        force_tick(&mut hbm);
        assert_eq!(hbm.current_seq(), 1);
        assert!(hbm.check_alive(), "missed=1, threshold=3 → alive");

        // Second tick → seq=2
        force_tick(&mut hbm);
        assert_eq!(hbm.current_seq(), 2);
        assert!(hbm.check_alive(), "missed=2, threshold=3 → alive");

        // Third tick → seq=3
        force_tick(&mut hbm);
        assert_eq!(hbm.current_seq(), 3);
        assert!(hbm.check_alive(), "missed=3, threshold=3 → alive (at threshold)");

        // Fourth tick → seq=4 → threshold exceeded
        force_tick(&mut hbm);
        assert!(!hbm.check_alive(), "missed=4, threshold=3 → dead");
    }

    #[test]
    fn test_heartbeat_ack_resets_missed() {
        let mut hbm = HeartbeatManager::new(Duration::from_secs(1));
        // We control last_tick directly to avoid wall-clock timing issues

        fn force_tick(hbm: &mut HeartbeatManager) {
            hbm.last_tick = Instant::now()
                .checked_sub(Duration::from_secs(60))
                .unwrap();
            assert!(hbm.tick(), "tick should fire after resetting last_tick");
        }

        // Simulate 3 ticks without ACK
        force_tick(&mut hbm);
        force_tick(&mut hbm);
        force_tick(&mut hbm);
        assert_eq!(hbm.current_seq(), 3);

        // Now we get an ACK for seq=2
        hbm.received_ack(2);
        assert!(hbm.check_alive(), "missed=1 → alive");

        // Two more ticks
        force_tick(&mut hbm);
        force_tick(&mut hbm);
        assert!(hbm.check_alive(), "missed=3 (threshold) → alive");

        // One more tick
        force_tick(&mut hbm);
        assert!(!hbm.check_alive(), "missed=4 (> threshold) → dead");
    }

    #[test]
    fn test_heartbeat_alive_on_start() {
        let hbm = HeartbeatManager::new(Duration::from_secs(1));
        assert!(hbm.check_alive(), "fresh manager should be alive");
        assert_eq!(hbm.current_seq(), 0);
    }
}
