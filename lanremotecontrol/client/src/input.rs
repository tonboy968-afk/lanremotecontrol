//! Client-side input capture module.
//!
//! Captures local keyboard and mouse events on the client machine using
//! Windows low-level APIs (`GetAsyncKeyState`, `GetCursorPos`) and converts
//! them into [`ControlCommandPayload`] events suitable for network
//! transmission to the host.
//!
//! # Platform Support
//!
//! All Win32 API calls are guarded with `#[cfg(windows)]`.  On non-Windows
//! platforms the module provides a stub that always returns empty event
//! vectors.

// ============================================================================
// Windows Implementation
// ============================================================================

#[cfg(windows)]
mod platform {
    use lanremotecontrol_common::*;
    use std::time::{SystemTime, UNIX_EPOCH};
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

    /// Captures local keyboard and mouse events on the client.
    ///
    /// Polls [`GetAsyncKeyState`] for all virtual key codes 0x01–0xFF to
    /// detect keyboard changes, tracks the cursor position with
    /// [`GetCursorPos`], and monitors mouse button states.
    pub struct InputCapture {
        /// Previous keyboard state (1 = down, 0 = up for each VK code).
        last_keyboard_state: [u8; 256],
        /// Last known cursor position in screen coordinates (x, y).
        last_cursor_pos: (i32, i32),
        /// Previous mouse button state (one entry per button index 0..4).
        last_button_state: [bool; 5],
        /// Monotonically increasing event sequence counter.
        seq_counter: u32,
    }

    impl InputCapture {
        /// Create a new `InputCapture` with all states initialised to "up".
        pub fn new() -> Self {
            Self {
                last_keyboard_state: [0u8; 256],
                last_cursor_pos: (0, 0),
                last_button_state: [false; 5],
                seq_counter: 0,
            }
        }

        /// Poll for all input changes since the last call.
        ///
        /// Compares the current state of every key (0x01–0xFF) and mouse
        /// button against the previously recorded state and produces a
        /// [`ControlCommandPayload`] for each detected transition.
        ///
        /// Also detects relative cursor movement since the last poll.
        pub fn poll(&mut self) -> Vec<ControlCommandPayload> {
            let mut events: Vec<ControlCommandPayload> = Vec::new();
            let now_us = timestamp_us();

            // ── Keyboard capture ──────────────────────────────────────────
            // Poll all virtual key codes from 0x01 to 0xFF.
            for vk in 1i32..=255i32 {
                // SAFETY: GetAsyncKeyState is safe to call with any VK code.
                // It returns the high-order bit = current down state.
                let state = unsafe { GetAsyncKeyState(vk) };
                let is_down = (state as u16) & 0x8000 != 0;
                let prev_down = self.last_keyboard_state[vk as usize] != 0;

                if is_down != prev_down {
                    self.last_keyboard_state[vk as usize] = if is_down { 1 } else { 0 };
                    let modifiers = get_modifier_state();
                    events.push(ControlCommandPayload::Key(KeyEvent {
                        key_code: vk as u32,
                        pressed: is_down,
                        modifiers,
                        timestamp_us: now_us,
                    }));
                }
            }

            // ── Mouse cursor position ─────────────────────────────────────
            let mut point = POINT::default();
            // SAFETY: GetCursorPos writes to POINT on the stack.
            if unsafe { GetCursorPos(&mut point) }.is_ok() {
                let new_pos = (point.x, point.y);
                let dx = new_pos.0.wrapping_sub(self.last_cursor_pos.0);
                let dy = new_pos.1.wrapping_sub(self.last_cursor_pos.1);

                if dx != 0 || dy != 0 {
                    self.last_cursor_pos = new_pos;
                    events.push(ControlCommandPayload::MouseMove(MouseMoveEvent {
                        dx,
                        dy,
                        abs_coords: false,
                        timestamp_us: now_us,
                    }));
                }
            }

            // ── Mouse buttons ─────────────────────────────────────────────
            // VK codes for mouse buttons:
            //   0x01 = VK_LBUTTON, 0x02 = VK_RBUTTON, 0x04 = VK_MBUTTON,
            //   0x05 = VK_XBUTTON1, 0x06 = VK_XBUTTON2
            let mouse_buttons = [
                (0x01i32, 0usize),
                (0x02, 1),
                (0x04, 2),
                (0x05, 3),
                (0x06, 4),
            ];

            for &(vk, button_idx) in &mouse_buttons {
                let state = unsafe { GetAsyncKeyState(vk) };
                let is_down = (state as u16) & 0x8000 != 0;
                let prev_down = self.last_button_state[button_idx];

                if is_down != prev_down {
                    self.last_button_state[button_idx] = is_down;
                    events.push(ControlCommandPayload::MouseButton(MouseButtonEvent {
                        button: button_idx as u8,
                        pressed: is_down,
                        x: self.last_cursor_pos.0,
                        y: self.last_cursor_pos.1,
                        timestamp_us: now_us,
                    }));
                }
            }

            events
        }

        /// Return the current sequence number and advance the counter.
        pub fn next_seq(&mut self) -> u32 {
            let seq = self.seq_counter;
            self.seq_counter = self.seq_counter.wrapping_add(1);
            seq
        }
    }

    // ── Helpers ─────────────────────────────────────────────────────────

    /// Read the current state of modifier keys (Shift, Ctrl, Alt, Win)
    /// and pack them into a bitmask.
    ///
    /// Bit layout:
    ///   bit 0 = Shift
    ///   bit 1 = Ctrl
    ///   bit 2 = Alt
    ///   bit 3 = Win (Left or Right)
    fn get_modifier_state() -> u8 {
        let mut mods = 0u8;

        // VK_SHIFT = 0x10
        if (unsafe { GetAsyncKeyState(0x10) } as u16) & 0x8000 != 0 {
            mods |= 0x01;
        }
        // VK_CONTROL = 0x11
        if (unsafe { GetAsyncKeyState(0x11) } as u16) & 0x8000 != 0 {
            mods |= 0x02;
        }
        // VK_MENU (Alt) = 0x12
        if (unsafe { GetAsyncKeyState(0x12) } as u16) & 0x8000 != 0 {
            mods |= 0x04;
        }
        // VK_LWIN = 0x5B, VK_RWIN = 0x5C
        if (unsafe { GetAsyncKeyState(0x5B) } as u16) & 0x8000 != 0
            || (unsafe { GetAsyncKeyState(0x5C) } as u16) & 0x8000 != 0
        {
            mods |= 0x08;
        }

        mods
    }

    /// Monotonic timestamp in microseconds since UNIX epoch.
    fn timestamp_us() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_micros() as u64
    }
}

// ============================================================================
// Non-Windows Stub
// ============================================================================

#[cfg(not(windows))]
mod platform {
    use lanremotecontrol_common::*;

    /// Stub implementation that always returns empty event vectors.
    pub struct InputCapture;

    impl InputCapture {
        pub fn new() -> Self {
            Self
        }

        pub fn poll(&mut self) -> Vec<ControlCommandPayload> {
            Vec::new()
        }

        pub fn next_seq(&mut self) -> u32 {
            0
        }
    }
}

// ============================================================================
// Re-export
// ============================================================================

/// Client-side input capture.
///
/// On non-Windows platforms, [`InputCapture::poll`] always returns an empty
/// vector.
pub use platform::InputCapture;
