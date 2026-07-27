# UI/UX Design - LAN Remote Control Software

## 1. Overview

This document defines the minimalist user interface design for the pure LAN PC remote control software. The design focuses on essential functionality with zero visual clutter, ensuring fast load times and an intuitive experience for both connection setup and active remote sessions.

---

## 2. Connection Panel UI Components

The connection panel is the entry point of the client application, displayed when no active session exists. It contains only the minimum required elements to establish a LAN connection.

### 2.1 Components

1. **IP Address Input Field**
   - Purpose: Enter the target host's local IP address (e.g., `192.168.1.100`)
   - Validation: Real-time IPv4 format validation; rejects invalid formats with subtle inline error state
   - Default state: Empty or placeholder text "Enter Host IP Address"

2. **Connect Button**
   - Purpose: Initiate the connection attempt to the specified host
   - States:
     - `Idle`: Enabled, primary visual emphasis (solid color)
     - `Connecting`: Disabled, shows loading indicator
     - `Error`: Enabled, shows error state if connection fails

3. **Status Indicator**
   - Purpose: Provide real-time feedback on connection state
   - States and Visual Cues:
     - `Disconnected`: Gray/neutral color, no active indicator
     - `Connecting`: Pulsing or animated blue/yellow indicator
     - `Connected`: Solid green indicator
     - `Error`: Red indicator with optional brief error message (e.g., "Host unreachable", "Authentication failed")

### 2.2 Layout

- Vertical stack layout, centered on the screen or application window
- Minimal padding, compact but accessible touch/click targets
- No unnecessary branding, menus, or settings panels in the initial view

---

## 3. Remote Control Window UI Components

The remote control window is displayed during an active session. It prioritizes maximum screen real estate for the remote display while providing essential controls.

### 3.1 Components

1. **Frame Display Area**
   - Purpose: Render the incoming remote screen frames
   - Behavior:
     - Fills the entire available window space
     - Maintains aspect ratio of the host screen (no stretching or distortion)
     - Black bars added on sides/top/bottom if client window aspect ratio differs from host

2. **Fullscreen Toggle Button**
   - Purpose: Switch between windowed and fullscreen modes
   - Location: Top-right corner of the remote control window (overlay, semi-transparent background)
   - States:
     - `Windowed Mode`: Icon shows "expand" or "maximize" symbol
     - `Fullscreen Mode`: Icon shows "collapse" or "restore" symbol

3. **Disconnect Button**
   - Purpose: Terminate the active remote control session
   - Location: Top-left corner of the remote control window (overlay, semi-transparent background)
   - Visual Design: Subtle icon (e.g., power symbol or "X"), only visible on hover or via a temporary toolbar that appears when mouse moves to the top edge

### 3.2 Layout

- Full-window frameless or minimal-title-bar window
- Overlay controls are non-intrusive, with auto-hide behavior after a short period of inactivity
- No status bars, menus, or secondary panels visible during active remote control unless explicitly triggered

---

## 4. Interaction Flow and State Transitions

### 4.1 Connection Flow

1. **Initial State**: Client app launches, displays the Connection Panel in `Disconnected` state.
2. **User Input**: User enters a valid IPv4 address in the IP input field.
3. **Connect Action**: User clicks the "Connect" button.
   - Transition: Connect button changes to `Connecting` state; Status Indicator shows `Connecting` (pulsing).
4. **Connection Attempt**: Client initiates UDP/TCP handshake with the host service.
5. **Success Path**:
   - Host accepts connection, session established.
   - Transition: Connection Panel closes or minimizes to tray; Remote Control Window opens in `Connected` state.
   - Status Indicator changes to `Connected` (solid green).
6. **Failure Path**:
   - Connection times out or host rejects request.
   - Transition: Connect button returns to `Idle` state; Status Indicator shows `Error` with brief error message. User can retry.

### 4.2 Remote Control Session Flow

1. **Session Start**: Remote Control Window opens, Frame Display Area begins rendering incoming screen frames.
2. **Active State**: 
   - Client receives and displays screen frames with ultra-low latency.
   - Keyboard/mouse input from client is forwarded to the host.
3. **Fullscreen Toggle**:
   - User clicks the Fullscreen Toggle button.
   - Transition: Window transitions to fullscreen mode (or back to windowed mode). Frame Display Area resizes to fill the new screen boundaries while maintaining aspect ratio.
4. **Disconnect Action**:
   - User clicks the Disconnect Button (or uses a global hotkey if configured).
   - Transition: Active session is terminated, host service receives disconnect signal.
   - Remote Control Window closes or minimizes to tray.
   - Client returns to Connection Panel in `Disconnected` state.

### 4.3 State Transition Summary

| Current State | Action | Next State | UI Changes |
|--------------|--------|------------|------------|
| Disconnected | Enter valid IP | Ready to Connect | IP field validated, Connect button enabled |
| Ready to Connect | Click Connect | Connecting | Connect button disabled + loading; Status = pulsing indicator |
| Connecting | Connection successful | Connected | Connection Panel closes/hides; Remote Control Window opens |
| Connecting | Connection fails | Disconnected (Error) | Connect button enabled; Status = red error indicator |
| Active Session | Click Fullscreen Toggle | Fullscreen Mode / Windowed Mode | Window style changes; Frame Display Area resizes |
| Active Session | Click Disconnect | Disconnected | Remote Control Window closes; Connection Panel shown |

---

## 5. Design Principles Summary

- **Minimalism**: Only essential controls are visible; secondary actions require minimal interaction (hover, edge swipe) to appear.
- **Clarity**: Status indicators use universally recognized color semantics (green = connected, red = error, blue/yellow = connecting).
- **Performance**: UI framework choices should prioritize low memory footprint and fast render times to avoid impacting the remote control performance.
- **Aspect Ratio Preservation**: Remote screen frames are never stretched or distorted; black bars are used when necessary to maintain the host's native aspect ratio.
