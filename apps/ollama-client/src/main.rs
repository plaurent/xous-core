#![cfg_attr(target_os = "none", no_std)]
#![cfg_attr(target_os = "none", no_main)]

mod config;
mod net;
mod predictor;
mod ui;

use num_traits::FromPrimitive;
use ui::OllamaClient;
use xous_ipc::Buffer;

/// Private name-server registration for this app's main server. Distinct from the
/// GAM app name (`gam::APP_NAME_OLLAMA_CLIENT`, generated from apps/manifest.json).
const OLLAMA_SERVER_NAME: &str = "User app 'ollama-client'";

/// Opcodes for the application main loop.
///
/// NOTE: `gotinput`/`redraw`/etc. callback discriminants must stay below 1000
/// (GAM reserves the rest for internal callbacks) — trivially true here.
#[derive(Debug, num_derive::FromPrimitive, num_derive::ToPrimitive)]
pub(crate) enum AppOp {
    /// redraw our screen
    Redraw,
    /// a completed input line committed from the IME text-entry area
    Line,
    /// handle raw key input (arrows / function keys), delivered alongside the IME
    Rawkeys,
    /// handle focus change
    FocusChange,
    /// a worker thread has an ollama reply (or error) ready for us
    ResponseReady,
    /// exit the application
    Quit,
}

fn main() -> ! {
    // Run on a generous stack; ureq's HTTP machinery is stack-hungry.
    let stack_size = 1024 * 1024;
    std::thread::Builder::new().stack_size(stack_size).spawn(wrapped_main).unwrap().join().unwrap()
}

fn wrapped_main() -> ! {
    log_server::init_wait().unwrap();
    log::set_max_level(log::LevelFilter::Info);
    log::info!("ollama-client PID is {}", xous::process::id());

    // Bump the heap ceiling — LLM replies and their word-wrapped copies can be large.
    const HEAP_LARGER_LIMIT: usize = 2048 * 1024;
    let result = xous::rsyscall(xous::SysCall::AdjustProcessLimit(
        xous::Limits::HeapMaximum as usize,
        0,
        HEAP_LARGER_LIMIT,
    ));
    if let Ok(xous::Result::Scalar2(1, current_limit)) = result {
        xous::rsyscall(xous::SysCall::AdjustProcessLimit(
            xous::Limits::HeapMaximum as usize,
            current_limit,
            HEAP_LARGER_LIMIT,
        ))
        .unwrap();
        log::info!("Heap limit increased to: {}", HEAP_LARGER_LIMIT);
    }

    let xns = xous_names::XousNames::new().unwrap();
    let sid = xns.register_name(OLLAMA_SERVER_NAME, None).expect("can't register server");

    let mut app = OllamaClient::new(sid);

    loop {
        let msg = xous::receive_message(sid).unwrap();
        match FromPrimitive::from_usize(msg.body.id()) {
            Some(AppOp::Redraw) => {
                app.redraw();
            }
            Some(AppOp::Line) => {
                let buffer = unsafe { Buffer::from_memory_message(msg.body.memory_message().unwrap()) };
                let s = buffer.to_original::<String, _>().unwrap();
                app.submit_line(&s);
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
            Some(AppOp::ResponseReady) => {
                app.on_response();
            }
            Some(AppOp::Quit) => {
                log::info!("ollama-client quitting");
                break;
            }
            _ => log::error!("ollama-client: unknown opcode {:?}", msg),
        }
    }

    xns.unregister_server(sid).unwrap();
    xous::destroy_server(sid).unwrap();
    xous::terminate_process(0)
}
