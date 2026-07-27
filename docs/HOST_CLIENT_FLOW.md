# Host Service and Client Application Logic Flows

## Overview
This document describes the lifecycle and interaction flow between the host service and the client application in LANRemoteControl. The flows cover host service startup, listening, connection handling, client connection initiation, session management, and disconnect/teardown procedures.

---

## 1. Host Service Lifecycle

### 1.1 Host Service Startup
1. **Initialization**: Host service starts (either as a system service or background process) and loads configuration settings (port range, authentication settings, encoding preferences).
2. **Network Listener Setup**: 
   - Creates UDP and/or TCP listeners on the configured port(s)
   - Binds to local IP addresses (typically all available interfaces: `0.0.0.0` or specific LAN IPs)
   - Applies TOS/QoS socket options for low-latency traffic prioritization
3. **Screen Capture Module Initialization**: 
   - Initializes OS-specific screen capture API (DXGI Desktop Duplication on Windows, ScreenCaptureKit on macOS, X11/Wayland on Linux)
   - Prepares encoding module with default compression strategy (LZ4 for raw delta or low-delay H.264/AV1)

### 1.2 Listening and Connection Handling
1. **Connection Request Reception**: Host service receives a connection request message (Type 0x05) from a client on the network listener.
2. **Authentication/Validation** (Optional): 
   - If authentication is enabled, host verifies the client's credentials (e.g., PIN or pre-shared key)
   - Rejects connection if authentication fails
3. **Capabilities Exchange**: 
   - Host sends its capabilities to the client (supported encodings, max resolution, screen dimensions, etc.)
   - Waits for client's capability confirmation
4. **Session Creation**: 
   - Upon successful negotiation, host creates a new session object
   - Initializes screen capture and encoding for the session
   - Starts sending initial screen frames to the client

---

## 2. Client Application Lifecycle

### 2.1 Client Connection Initiation
1. **User Input**: User enters the host IP address/hostname and clicks "Connect" in the Connection Panel.
2. **Connection Attempt**: 
   - Client creates network sockets (UDP/TCP) and attempts to connect to the host's IP and port
   - Sends connection request message (Type 0x05) to the host
3. **Waiting for Response**: 
   - Client displays "Connecting..." status indicator
   - Waits for host's capabilities response or connection rejection

### 2.2 Session Management
1. **Connection Success**: 
   - Client receives host's capabilities and sends confirmation
   - Host begins sending screen frames
   - Client opens the Remote Control Window and starts rendering incoming frames
   - Status indicator changes to "Connected" (Green)
2. **Active Session Operations**:
   - **Screen Frame Reception**: Client continuously receives, decodes, and renders screen frames from the host
   - **Input Event Transmission**: Client captures local keyboard/mouse events and forwards them to the host with timestamps
   - **Heartbeat Maintenance**: Both client and host send periodic heartbeat messages to maintain connection state
3. **Performance Monitoring**: 
   - Client monitors network RTT, packet loss, and frame rendering latency
   - Adjusts encoding strategy or requests full keyframes if needed

---

## 3. Disconnect and Teardown Procedures

### 3.1 User-Initiated Disconnect
1. **Disconnect Request**: User clicks the "Disconnect" button in the Remote Control Window or closes the window.
2. **Teardown Message**: Client sends a teardown message (Type 0x05 with teardown flag) to the host.
3. **Host Teardown**:
   - Host stops screen capture and encoding for the session
   - Closes network connections associated with the session
   - Frees resources (memory, encoder instances, capture handles)
4. **Client Cleanup**:
   - Client closes the Remote Control Window
   - Resets to Connection Panel state
   - Clears session data and prepares for new connection

### 3.2 Host-Initiated Disconnect or Error Conditions
1. **Host Shutdown/Restart**: If the host service is stopped or restarted, it closes all active sessions and sends disconnect notifications to clients (if possible).
2. **Network Failure**: 
   - If heartbeats are missed for a configured threshold (e.g., 3-5 consecutive heartbeats without response), the host assumes the client connection is lost
   - Host initiates teardown procedures for the session
3. **Authentication Failure During Session**: If authentication expires or becomes invalid during an active session, host can initiate disconnect with appropriate error code.

### 3.3 Graceful vs Forceful Teardown
- **Graceful Teardown**: Both client and host exchange teardown messages, ensuring clean resource release on both sides
- **Forceful Teardown**: If a side detects a critical error or timeout, it may close sockets immediately without waiting for acknowledgment, relying on OS-level socket cleanup

---

## 4. Flow Summary Diagram

```
Host Service Flow:
[Startup] -> [Setup Network Listener] -> [Listen for Connection Requests]
     |                                          |
     v                                          v
[Send Capabilities] <- [Receive Connection Request]
     |                                          |
     v                                          v
[Create Session] -> [Start Screen Capture/Encoding] -> [Send Frames / Receive Input]

Client Application Flow:
[User Enters IP] -> [Click Connect] -> [Send Connection Request]
     |                                              |
     v                                              v
[Show Connecting Status] <- [Wait for Host Response]
     |                                              |
     v                                              v
[Open Remote Control Window] -> [Render Frames / Send Input] -> [Active Session]

Disconnect Flow (Both Sides):
[Disconnect Request] -> [Send Teardown Message] -> [Stop Capture/Encoding or Close Window]
     |                                                    |
     v                                                    v
[Close Network Connections] <- [Receive Teardown Ack] -> [Clean Up Resources]
```

---

## 5. Key Design Considerations

- **State Management**: Both host and client maintain clear state machines for connection lifecycle (Idle -> Connecting -> Connected -> Disconnecting -> Disconnected)
- **Resource Cleanup**: Ensure all captured resources (screen handles, encoder instances, network sockets) are properly released during teardown to prevent memory leaks
- **Reconnection Support**: While not required for initial MVP, the flow design should support seamless reconnection if temporary network drops occur, by preserving session state where possible