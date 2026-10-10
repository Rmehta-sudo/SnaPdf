use std::sync::mpsc::{self, Receiver};

/// Incremental scale changes emitted by a native trackpad pinch.
pub struct NativePinch {
    pub events: Receiver<NativePinchEvent>,
}

pub struct NativePinchEvent {
    pub factor: f32,
}

#[cfg(target_os = "linux")]
pub fn listen(creation: &eframe::CreationContext<'_>) -> Option<NativePinch> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use x11rb::connection::Connection as _;
    use x11rb::protocol::Event;
    use x11rb::protocol::xinput::{ConnectionExt as _, Device, EventMask, XIEventMask};

    let window = match creation.window_handle().ok()?.as_raw() {
        RawWindowHandle::Xlib(handle) => handle.window as u32,
        RawWindowHandle::Xcb(handle) => handle.window.get(),
        _ => return None,
    };
    let repaint = creation.egui_ctx.clone();
    let (sender, events) = mpsc::channel();

    std::thread::Builder::new()
        .name("snappdf-x11-pinch".to_owned())
        .spawn(move || {
            let Ok((connection, _screen)) = x11rb::connect(None) else {
                return;
            };
            let Ok(version) = connection.xinput_xi_query_version(2, 4) else {
                return;
            };
            let Ok(version) = version.reply() else {
                return;
            };
            if (version.major_version, version.minor_version) < (2, 4) {
                return;
            }

            // x11rb 0.13 parses XI 2.4 gesture events but does not yet expose
            // named mask constants for bits 27-29.
            let gesture_mask = XIEventMask::from((1_u32 << 27) | (1_u32 << 28) | (1_u32 << 29));
            let masks = [EventMask {
                deviceid: Device::ALL.into(),
                mask: vec![gesture_mask],
            }];
            let Ok(cookie) = connection.xinput_xi_select_events(window, &masks) else {
                return;
            };
            if cookie.check().is_err() || connection.flush().is_err() {
                return;
            }

            let mut previous_scale = 1.0_f32;
            while let Ok(event) = connection.wait_for_event() {
                match event {
                    Event::XinputGesturePinchBegin(_) => previous_scale = 1.0,
                    Event::XinputGesturePinchUpdate(event) => {
                        let scale = event.scale as f32 / 65_536.0;
                        if scale.is_finite() && scale > 0.0 {
                            let factor = (scale / previous_scale).clamp(0.5, 2.0);
                            previous_scale = scale;
                            if (factor - 1.0).abs() > 0.0001 {
                                if sender.send(NativePinchEvent { factor }).is_err() {
                                    break;
                                }
                                repaint.request_repaint();
                            }
                        }
                    }
                    Event::XinputGesturePinchEnd(_) => previous_scale = 1.0,
                    _ => {}
                }
            }
        })
        .ok()?;

    Some(NativePinch { events })
}

#[cfg(not(target_os = "linux"))]
pub fn listen(_creation: &eframe::CreationContext<'_>) -> Option<NativePinch> {
    None
}
