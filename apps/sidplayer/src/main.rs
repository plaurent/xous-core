mod catalog;
mod cpu6502;
mod netfetch;
mod player;
mod psid;
mod sid;
mod sidplayer;

use num_traits::FromPrimitive;
use sidplayer::SidPlayer;

/// Private name-server registration for this app's main server. Distinct from the
/// GAM app name (`gam::APP_NAME_SIDPLAYER`, generated from apps/manifest.json).
const SIDPLAYER_SERVER_NAME: &str = "User app 'sidplayer'";

/// Opcodes for the application main loop.
#[derive(Debug, num_derive::FromPrimitive, num_derive::ToPrimitive)]
pub(crate) enum AppOp {
    /// redraw our screen
    Redraw,
    /// handle raw key input
    Rawkeys,
    /// handle focus change
    FocusChange,
    /// exit the application
    Quit,
    /// the codec wants more audio frames (delivered via hook_frame_callback)
    AudioFrame,
}

fn main() -> ! {
    log_server::init_wait().unwrap();
    log::set_max_level(log::LevelFilter::Info);
    log::info!("sidplayer PID is {}", xous::process::id());

    let xns = xous_names::XousNames::new().unwrap();
    let sid = xns.register_name(SIDPLAYER_SERVER_NAME, None).expect("can't register server");

    let mut app = SidPlayer::new(sid);

    loop {
        let msg = xous::receive_message(sid).unwrap();
        match FromPrimitive::from_usize(msg.body.id()) {
            Some(AppOp::Redraw) => {
                app.redraw();
            }
            Some(AppOp::Rawkeys) => xous::msg_scalar_unpack!(msg, k1, k2, k3, k4, {
                let keys = [
                    core::char::from_u32(k1 as u32).unwrap_or('\u{0000}'),
                    core::char::from_u32(k2 as u32).unwrap_or('\u{0000}'),
                    core::char::from_u32(k3 as u32).unwrap_or('\u{0000}'),
                    core::char::from_u32(k4 as u32).unwrap_or('\u{0000}'),
                ];
                for k in keys.iter() {
                    if *k != '\u{0000}' {
                        app.key(*k);
                    }
                }
            }),
            Some(AppOp::FocusChange) => xous::msg_scalar_unpack!(msg, new_state_code, _, _, _, {
                let new_state = gam::FocusState::convert_focus_change(new_state_code);
                app.on_focus(matches!(new_state, gam::FocusState::Foreground));
            }),
            Some(AppOp::AudioFrame) => xous::msg_scalar_unpack!(msg, free_play, _avail, _, routing_id, {
                if routing_id == codec::AUDIO_CB_ROUTING_ID {
                    app.audio_frame(free_play);
                }
            }),
            Some(AppOp::Quit) => {
                log::info!("sidplayer quitting");
                break;
            }
            _ => log::error!("sidplayer: unknown opcode {:?}", msg),
        }
    }

    xns.unregister_server(sid).unwrap();
    xous::destroy_server(sid).unwrap();
    xous::terminate_process(0)
}
