# LANRemoteControl Network Protocol Design

## Overview
LANRemoteControl uses a custom UDP-based network protocol optimized for low latency and reliability on local area networks (LAN). The protocol is designed to minimize overhead while ensuring timely delivery of critical control commands and screen frames.

## Protocol Characteristics

- **Transport Layer**: UDP (User Datagram Protocol)
- **Port Configuration**: Configurable port range (default: 50000-50010 for multiplexing)
- **LAN Optimization**: Designed exclusively for local network environments with typical latencies < 1ms and high bandwidth (> 1 Gbps)
- **Packet Size**: Optimized for MTU sizes (typically 1500 bytes for Ethernet, with fragmentation handled at the application layer if needed)

## Message Types

The protocol defines the following message types:

### 1. Control Commands (Type: 0x01)
- Used for client-to-host input events (keyboard, mouse)
- Contains timestamped input event data
- Requires ACK confirmation

### 2. Screen Frames (Type: 0x02)
- Used for host-to-client screen frame data
- Contains compressed delta frames or full frames
- May be lossy or lossless depending on encoding strategy
- Does not require per-frame ACK (reliability handled at application level for critical frames)

### 3. ACK Messages (Type: 0x03)
- Acknowledgment messages for control commands and critical screen frames
- Contains sequence number of the acknowledged message

### 4. Heartbeats (Type: 0x04)
- Periodic keep-alive messages to maintain connection state
- Sent by both host and client
- No ACK required

### 5. Connection Management (Type: 0x05)
- Used for session initialization, negotiation, and teardown
- Includes capabilities exchange (supported encodings, max resolution, etc.)

## Message Format

Each message follows this binary format:

```
+-------------------+-------------------+-------------------+-------------------+
| Message Type (1B) | Sequence Number (4B)| Payload Length (4B)| Reserved (2B)     |
+-------------------+-------------------+-------------------+-------------------+
| Payload (Variable Length, up to MTU - 11 bytes)                              |
+--------------------------------------------------------------------------------+
```

### Field Descriptions:
- **Message Type (1 byte)**: Identifies the message type (0x01-0x05 as defined above)
- **Sequence Number (4 bytes)**: Little-endian unsigned 32-bit integer, increments per message stream
- **Payload Length (4 bytes)**: Little-endian unsigned 32-bit integer, length of payload in bytes
- **Reserved (2 bytes)**: Reserved for future use, must be set to 0
- **Payload (Variable)**: Message-specific data

## Sequence Number and ACK Mechanism

### Sequence Numbers
- Each message stream (control commands, screen frames) maintains its own sequence number space
- Sequence numbers are 32-bit unsigned integers that wrap around after reaching MAX_UINT32
- Host and client maintain separate sequence number counters for each direction

### ACK Mechanism
- **Control Commands**: Every control command message (Type 0x01) must be acknowledged with an ACK message (Type 0x03)
- **Critical Screen Frames**: Certain screen frames (e.g., first frame after resolution change, or frames marked as key frames) require ACK confirmation
- **ACK Timeout and Retransmission**: If an ACK is not received within a configurable timeout (default: 50ms for LAN), the message is retransmitted up to a maximum number of retries (default: 3)

### Lost Packet Handling
- For screen frames, lost packets are generally not retransmitted to maintain low latency; instead, the next delta frame will compensate
- For control commands and critical data, retransmission is implemented with exponential backoff

## TOS/QoS Settings for Low Delay

To ensure low latency for remote control traffic, the protocol leverages IP Layer QoS mechanisms:

### Type of Service (TOS) / Traffic Class
- **Control Commands and ACKs**: Set DSCP to AF41 (Assured Forwarding 41) or EF (Expedited Forwarding) depending on OS support, prioritizing low latency for input events
- **Heartbeats**: Set DSCP to CS6 (Class Selector 6) for network management priority
- **Screen Frames**: Set DSCP to AF31 or BE (Best Effort) depending on whether the frame is a key frame or delta frame

### Socket Options
- **IP_TOS / IP_TCLASS**: Set socket options to apply DSCP markings
- **TCP_NODELAY equivalent for UDP**: Minimize buffering by disabling any application-level queuing; send packets immediately
- **Socket Buffer Sizing**: Optimize send and receive buffer sizes to match LAN bandwidth-delay product while avoiding excessive queuing delay

## Connection Establishment

1. **Discovery Phase** (Optional): Client may broadcast a discovery message on the local network to find available hosts
2. **Connection Initiation**: Client sends a connection request message (Type 0x05) to the host's IP and port
3. **Capabilities Exchange**: Host responds with its capabilities (supported encodings, max resolution, etc.)
4. **Session Initialization**: Client confirms capabilities and session begins; sequence numbers are synchronized

## Security Considerations (LAN Scope)

Since this is a pure LAN solution:
- Authentication can be implemented at the application layer (e.g., pre-shared key or PIN verification during connection establishment)
- Encryption is optional but recommended for sensitive environments; could use lightweight authenticated encryption if needed
