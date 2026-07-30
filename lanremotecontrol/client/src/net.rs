//! Network module for the LANRemoteControl client application.
//!
//! Provides a UDP client and a handshake helper for connecting to the host.

use std::io;
use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::time::Duration;

use lanremotecontrol_common::*;

// ============================================================================
// UdpClient
// ============================================================================

/// A connected UDP socket used to communicate with the host.
pub struct UdpClient {
    socket: UdpSocket,
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

        Ok(Self { socket })
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
