//! Game controllers through gilrs (Windows.Gaming.Input), on a thread of their own. SDL mappings
//! are left out so a raw controller's buttons keep the device's own order, the order the browser
//! Gamepad API numbers them in; see `sc_app::bezel` for how ids and numbers line up.

use gilrs::{Button, EventType, Gamepad, GamepadId, GilrsBuilder, ev::Code};
use tokio::sync::mpsc::UnboundedSender;

use sc_app::bezel::{Pad, PadButton};

use super::EngineMsg;

fn info(id: GamepadId, pad: &Gamepad<'_>) -> Pad {
    Pad {
        id: usize::from(id),
        name: pad.os_name().to_owned(),
        vendor: pad.vendor_id(),
        product: pad.product_id(),
        // gilrs gives a WGI Gamepad (the XInput class) the nil uuid.
        standard: pad.uuid() == [0; 16],
    }
}

/// The device's own button index: the low 16 bits of a button code (kind 0).
fn raw(code: Code) -> Option<u32> {
    let c = code.into_u32();
    (c >> 16 == 0).then_some(c & 0xffff)
}

/// The W3C standard gamepad index of a gilrs button.
fn standard(button: Button) -> Option<u32> {
    Some(match button {
        Button::South => 0,
        Button::East => 1,
        Button::West => 2,
        Button::North => 3,
        Button::LeftTrigger => 4,
        Button::RightTrigger => 5,
        Button::LeftTrigger2 => 6,
        Button::RightTrigger2 => 7,
        Button::Select => 8,
        Button::Start => 9,
        Button::LeftThumb => 10,
        Button::RightThumb => 11,
        Button::DPadUp => 12,
        Button::DPadDown => 13,
        Button::DPadLeft => 14,
        Button::DPadRight => 15,
        Button::Mode => 16,
        _ => return None,
    })
}

pub fn spawn(tx: UnboundedSender<EngineMsg>) {
    std::thread::spawn(move || {
        let built = GilrsBuilder::new()
            .with_default_filters(false)
            .add_included_mappings(false)
            .add_env_mappings(false)
            .build();
        let mut gilrs = match built {
            Ok(g) => g,
            Err(e) => {
                tracing::warn!("no game controllers: {e}");
                return;
            }
        };
        for (id, pad) in gilrs.gamepads() {
            tracing::info!("controller {}: {}", usize::from(id), pad.os_name());
            let _ = tx.send(EngineMsg::Connected(info(id, &pad)));
        }
        while !tx.is_closed() {
            let Some(ev) = gilrs.next_event_blocking(Some(std::time::Duration::from_secs(1)))
            else {
                continue;
            };
            let id = ev.id;
            let edge = |button, code, pressed| {
                EngineMsg::Button(PadButton {
                    pad: usize::from(id),
                    raw: raw(code),
                    standard: standard(button),
                    pressed,
                })
            };
            let msg = match ev.event {
                EventType::Connected => {
                    let pad = info(id, &gilrs.gamepad(id));
                    tracing::info!("controller {} connected: {}", pad.id, pad.name);
                    EngineMsg::Connected(pad)
                }
                EventType::Disconnected => EngineMsg::Disconnected(usize::from(id)),
                EventType::ButtonPressed(b, code) => edge(b, code, true),
                EventType::ButtonReleased(b, code) => edge(b, code, false),
                _ => continue,
            };
            let _ = tx.send(msg);
        }
    });
}
