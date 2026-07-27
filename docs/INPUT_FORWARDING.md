# Keyboard and Mouse Input Forwarding Design

## Overview
The input forwarding mechanism in LANRemoteControl captures local keyboard and mouse events on the client side, timestamps them, and forwards them to the host service over the LAN network. The host service then injects these events into the host OS using low-level input APIs with minimal latency.

## 1. Client-Side Input Event Capture

### 1.1 Keyboard Events
- **Capture Mechanism**: 
  - Windows: `GetAsyncKeyState()`, `GetKeyboardState()`, or window message hooks (`WH_KEYBOARD_LL`)
  - macOS: Core Graphics `CGEventCreateKeyboardEvent()` and event tap callbacks
  - Linux: X11 `XQueryKeymap()` or `/dev/input/event*` via evdev for Wayland/X11
- **Event Data**: Key code (scan code or virtual key code), key state (down/up/repeat), modifier states (Shift, Ctrl, Alt, Win)

### 1.2 Mouse Events
- **Capture Mechanism**:
  - Windows: `GetCursorPos()`, `GetMouseState()`, or low-level mouse hooks (`WH_MOUSE_LL`)
  - macOS: Core Graphics event taps for mouse movement and button states
  - Linux: X11 `XQueryPointer()` or `/dev/input/event*` for evdev
- **Event Data**: Mouse position (absolute screen coordinates or relative delta), button states (left, right, middle, extra buttons), scroll wheel delta

### 1.3 Event Bundling and Compression
To minimize network overhead while preserving precision:
- **Coalescing**: Multiple rapid key events or small mouse movements within a short time window (e.g., 2-4ms) are coalesced into a single network message.
- **Delta Encoding for Mouse Movement**: Instead of sending absolute cursor positions continuously, send relative delta movements when the cursor is in motion, switching to absolute position on button clicks or when the movement threshold is exceeded.

---

## 2. Host-Side Low-Level Input Injection

### 2.1 Windows
- **Primary API**: `SendInput()` function
  - Supports keyboard and mouse event injection at the lowest system level before filter drivers
  - Can simulate hardware-level input events, ensuring compatibility with all applications including games and fullscreen apps
- **Alternative APIs**: 
  - `mouse_event()`, `keybd_event()` (legacy, deprecated but still functional)
  - UI Automation or PostMessage for specific application targeting (not recommended for low-latency general injection)

### 2.2 macOS
- **Primary API**: Core Graphics `CGEventPost(kCGHIDEventTap, event)`
  - Requires Accessibility permissions for full input simulation
  - Supports keyboard and mouse event injection with proper timestamping
- **Permissions**: Must request and obtain Accessibility access in System Preferences > Security & Privacy > Privacy

### 2.3 Linux
- **X11**: `XSendEvent()` or `XTestFakeKeyEvent()`, `XTestFakeMotionEvent()` via XTest extension
- **evdev (Wayland/headless)**: Write directly to `/dev/input/event*` devices using `write()` system calls with `input_event` structures
  - Requires root privileges or appropriate udev rules for input device access

---

## 3. Timestamping and Ordering Rules for Input Events

### 3.1 Timestamp Source
- **Client-Side Timestamps**: Capture the exact time of input event generation using high-resolution timers:
  - Windows: `QueryPerformanceCounter()` or `GetSystemTimePreciseAsFileTime()`
  - macOS: `mach_absolute_time()` or `clock_gettime(CLOCK_MONOTONIC)`
  - Linux: `clock_gettime(CLOCK_MONOTONIC)`
- **Timestamp Format**: 64-bit integer representing microseconds or nanoseconds since a fixed epoch (e.g., process start or Unix epoch)

### 3.2 Network Transmission Order
- Input events are transmitted in the exact order they were captured on the client side.
- Each input event message includes:
  - Event type (keyboard down/up, mouse move/button/scroll)
  - Timestamp (client-side capture time)
  - Event data (key codes, mouse coordinates/buttons, scroll deltas)

### 3.3 Host-Side Reordering and Jitter Compensation
- **Out-of-Order Detection**: If the host receives an input event with a timestamp older than the last processed event's timestamp by more than a threshold (e.g., 10ms), it may indicate network reordering or jitter.
- **Jitter Buffer**: A small jitter buffer (2-5ms) is used on the host side to reorder events and smooth out network latency variations, ensuring input feels natural and responsive without stuttering.
- **Drop Policy**: Events that are too old (e.g., timestamp > 100ms older than current host time) are dropped to prevent stale input from affecting the remote session.

### 3.4 Synchronization with Screen Frames
- Input events are processed by the host injection module immediately upon receipt, without waiting for specific screen frames.
- The client and host maintain independent timing; the visual feedback on the client side (cursor movement, key highlighting) is local and immediate, while the actual effect on the remote host depends on network latency and host processing time.

---

## 4. Security and Privacy Considerations

- **Input Logging**: The software should not log or store input events beyond the session lifetime to protect user privacy.
- **Injection Restrictions**: On the host side, input injection should only affect the active display/session and should not interfere with local user input if a local user is present (optional: detect local input activity and pause or queue remote input).
- **Permission Requirements**: Clearly document the required OS-level permissions (e.g., Accessibility on macOS, screen recording/input simulation permissions) for users to configure their systems properly.
