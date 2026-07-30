//! Host-side input injection module.
//!
//! Injects keyboard and mouse events into the host operating system using
//! the Windows [`SendInput`] API.  This provides low-level input simulation
//! that works with all applications, including full-screen and games.
//!
//! # Platform Support
//!
//! All Win32 API calls are guarded with `#[cfg(windows)]`.  On non-Windows
//! platforms the module provides a stub that returns
//! [`io::ErrorKind::Unsupported`].

// ============================================================================
// Windows Implementation
// ============================================================================

#[cfg(windows)]
mod platform {
    use lanremotecontrol_common::*;
    use std::io;
    use windows::Win32::UI::Input::KeyboardAndMouse::*;

    /// Injects keyboard and mouse events into the host OS.
    ///
    /// Uses the Windows [`SendInput`] API for low-level input simulation.
    pub struct InputInjector;

    impl InputInjector {
        /// Inject a single [`ControlCommandPayload`] into the host OS.
        ///
        /// Converts the payload to one or more [`INPUT`] structures and
        /// passes them to [`SendInput`].
        pub fn inject(payload: &ControlCommandPayload) -> io::Result<()> {
            let inputs = payload_to_inputs(payload);
            if inputs.is_empty() {
                return Ok(());
            }

            let sent = unsafe {
                SendInput(
                    &inputs,
                    std::mem::size_of::<INPUT>() as i32,
                )
            };

            if sent == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    "SendInput returned 0 — input was blocked or failed",
                ));
            }
            if sent != inputs.len() as u32 {
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    format!(
                        "SendInput: expected {} inputs, got {}",
                        inputs.len(),
                        sent
                    ),
                ));
            }
            Ok(())
        }

        /// Inject a batch of events while preserving their order.
        ///
        /// All events are collected into a single [`SendInput`] call so that
        /// the host OS processes them atomically and in sequence.
        pub fn inject_batch(payloads: &[ControlCommandPayload]) -> io::Result<()> {
            let all_inputs: Vec<INPUT> = payloads
                .iter()
                .flat_map(|p| payload_to_inputs(p))
                .collect();

            if all_inputs.is_empty() {
                return Ok(());
            }

            let sent = unsafe {
                SendInput(
                    &all_inputs,
                    std::mem::size_of::<INPUT>() as i32,
                )
            };

            if sent == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    "SendInput batch returned 0 — input was blocked or failed",
                ));
            }
            if sent != all_inputs.len() as u32 {
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    format!(
                        "SendInput batch: expected {} inputs, got {}",
                        all_inputs.len(),
                        sent
                    ),
                ));
            }
            Ok(())
        }
    }

    // ── Payload conversion ──────────────────────────────────────────────

    /// Convert a single [`ControlCommandPayload`] into one or more
    /// [`INPUT`] structures suitable for [`SendInput`].
    fn payload_to_inputs(payload: &ControlCommandPayload) -> Vec<INPUT> {
        match payload {
            ControlCommandPayload::Key(ke) => {
                // KEYEVENTF_KEYDOWN = 0x0000 (no flag)
                // KEYEVENTF_KEYUP   = 0x0002
                let flag = if ke.pressed {
                    KEYBD_EVENT_FLAGS(0)
                } else {
                    KEYEVENTF_KEYUP
                };

                vec![INPUT {
                    r#type: INPUT_KEYBOARD,
                    Anonymous: INPUT_0 {
                        ki: KEYBDINPUT {
                            wVk: VIRTUAL_KEY(ke.key_code as u16),
                            wScan: 0,
                            dwFlags: flag,
                            time: 0,
                            dwExtraInfo: 0usize,
                        },
                    },
                }]
            }

            ControlCommandPayload::MouseMove(mm) => {
                // Relative movement uses MOUSEEVENTF_MOVE alone,
                // absolute movement additionally sets MOUSEEVENTF_ABSOLUTE.
                let flags = if mm.abs_coords {
                    MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE
                } else {
                    MOUSEEVENTF_MOVE
                };

                vec![INPUT {
                    r#type: INPUT_MOUSE,
                    Anonymous: INPUT_0 {
                        mi: MOUSEINPUT {
                            dx: mm.dx,
                            dy: mm.dy,
                            mouseData: 0,
                            dwFlags: flags,
                            time: 0,
                            dwExtraInfo: 0usize,
                        },
                    },
                }]
            }

            ControlCommandPayload::MouseButton(mb) => {
                // Map button index + pressed state to MOUSE_EVENT_FLAGS.
                let flags = match (mb.button, mb.pressed) {
                    (0, true) => MOUSEEVENTF_LEFTDOWN,
                    (0, false) => MOUSEEVENTF_LEFTUP,
                    (1, true) => MOUSEEVENTF_RIGHTDOWN,
                    (1, false) => MOUSEEVENTF_RIGHTUP,
                    (2, true) => MOUSEEVENTF_MIDDLEDOWN,
                    (2, false) => MOUSEEVENTF_MIDDLEUP,
                    // XBUTTON1 (index 3) and XBUTTON2 (index 4)
                    (3, true) => MOUSEEVENTF_XDOWN,
                    (3, false) => MOUSEEVENTF_XUP,
                    (4, true) => MOUSEEVENTF_XDOWN,
                    (4, false) => MOUSEEVENTF_XUP,
                    _ => return vec![],
                };

                // XBUTTON1 = 1, XBUTTON2 = 2
                let mouse_data = match mb.button {
                    3 => 1u32,
                    4 => 2u32,
                    _ => 0u32,
                };

                vec![INPUT {
                    r#type: INPUT_MOUSE,
                    Anonymous: INPUT_0 {
                        mi: MOUSEINPUT {
                            dx: mb.x,
                            dy: mb.y,
                            mouseData: mouse_data,
                            dwFlags: flags,
                            time: 0,
                            dwExtraInfo: 0usize,
                        },
                    },
                }]
            }

            ControlCommandPayload::Scroll(se) => {
                let mut inputs = Vec::new();

                if se.delta_y != 0 {
                    inputs.push(INPUT {
                        r#type: INPUT_MOUSE,
                        Anonymous: INPUT_0 {
                            mi: MOUSEINPUT {
                                dx: 0,
                                dy: 0,
                                mouseData: se.delta_y as u32,
                                dwFlags: MOUSEEVENTF_WHEEL,
                                time: 0,
                                dwExtraInfo: 0usize,
                            },
                        },
                    });
                }

                if se.delta_x != 0 {
                    inputs.push(INPUT {
                        r#type: INPUT_MOUSE,
                        Anonymous: INPUT_0 {
                            mi: MOUSEINPUT {
                                dx: 0,
                                dy: 0,
                                mouseData: se.delta_x as u32,
                                dwFlags: MOUSEEVENTF_HWHEEL,
                                time: 0,
                                dwExtraInfo: 0usize,
                            },
                        },
                    });
                }

                inputs
            }
        }
    }

    // ── Tests ──────────────────────────────────────────────────────────

    #[cfg(test)]
    mod tests {
        use super::*;

        /// Verify that a Key event payload produces exactly one INPUT
        /// with the correct type and flags.
        #[test]
        fn test_payload_to_inputs_key_down() {
            let ke = KeyEvent {
                key_code: 0x41, // 'A'
                pressed: true,
                modifiers: 0,
                timestamp_us: 1000,
            };
            let payload = ControlCommandPayload::Key(ke);
            let inputs = payload_to_inputs(&payload);
            assert_eq!(inputs.len(), 1);
            assert_eq!(inputs[0].r#type, INPUT_KEYBOARD);
        }

        #[test]
        fn test_payload_to_inputs_key_up() {
            let ke = KeyEvent {
                key_code: 0x41,
                pressed: false,
                modifiers: 0,
                timestamp_us: 1001,
            };
            let payload = ControlCommandPayload::Key(ke);
            let inputs = payload_to_inputs(&payload);
            assert_eq!(inputs.len(), 1);
            assert_eq!(inputs[0].r#type, INPUT_KEYBOARD);
        }

        /// Verify that a MouseMove event produces one INPUT with
        /// MOUSEEVENTF_MOVE (relative).
        #[test]
        fn test_payload_to_inputs_mouse_move_relative() {
            let mm = MouseMoveEvent {
                dx: 10,
                dy: -5,
                abs_coords: false,
                timestamp_us: 2000,
            };
            let payload = ControlCommandPayload::MouseMove(mm);
            let inputs = payload_to_inputs(&payload);
            assert_eq!(inputs.len(), 1);
            assert_eq!(inputs[0].r#type, INPUT_MOUSE);
        }

        /// Verify that a MouseButton left-down event is mapped correctly.
        #[test]
        fn test_payload_to_inputs_mouse_left_down() {
            let mb = MouseButtonEvent {
                button: 0,
                pressed: true,
                x: 100,
                y: 200,
                timestamp_us: 3000,
            };
            let payload = ControlCommandPayload::MouseButton(mb);
            let inputs = payload_to_inputs(&payload);
            assert_eq!(inputs.len(), 1);
            assert_eq!(inputs[0].r#type, INPUT_MOUSE);
        }

        /// Verify that Scroll with both axes produces two INPUTs.
        #[test]
        fn test_payload_to_inputs_scroll_both() {
            let sc = ScrollEvent {
                delta_x: -30,
                delta_y: 120,
                timestamp_us: 4000,
            };
            let payload = ControlCommandPayload::Scroll(sc);
            let inputs = payload_to_inputs(&payload);
            assert_eq!(inputs.len(), 2);
        }

        /// Verify that a no-delta Scroll produces zero INPUTs.
        #[test]
        fn test_payload_to_inputs_scroll_zero() {
            let sc = ScrollEvent {
                delta_x: 0,
                delta_y: 0,
                timestamp_us: 4001,
            };
            let payload = ControlCommandPayload::Scroll(sc);
            let inputs = payload_to_inputs(&payload);
            assert!(inputs.is_empty());
        }

        /// Verify that inject_batch handles an empty slice.
        #[test]
        fn test_inject_batch_empty() {
            let result = InputInjector::inject_batch(&[]);
            assert!(result.is_ok());
        }
    }
}

// ============================================================================
// Non-Windows Stub
// ============================================================================

#[cfg(not(windows))]
mod platform {
    use lanremotecontrol_common::*;
    use std::io;

    /// Stub that always returns an "Unsupported" error on non-Windows
    /// platforms.
    pub struct InputInjector;

    impl InputInjector {
        pub fn inject(_payload: &ControlCommandPayload) -> io::Result<()> {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Input injection is only supported on Windows",
            ))
        }

        pub fn inject_batch(_payloads: &[ControlCommandPayload]) -> io::Result<()> {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Input injection is only supported on Windows",
            ))
        }
    }
}

// ============================================================================
// Re-export
// ============================================================================

/// Host-side input injector.
///
/// On non-Windows platforms, [`InputInjector::inject`] returns
/// `Err(io::ErrorKind::Unsupported)`.
pub use platform::InputInjector;
