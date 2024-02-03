#![cfg_attr(target_os = "none", no_std)]
#![cfg_attr(target_os = "none", no_main)]

mod book;
use book::*;
use num_traits::*;
use xous::Message;

// This name should be (1) unique (2) under 64 characters long and (3) ideally descriptive.
const BOOK_SERVER_NAME: &'static str = "User app 'book'";

/// Opcodes for the application main loop
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
}

const BOOK_UPDATE_RATE_MS: usize = 50;

fn main() -> ! {
    log_server::init_wait().unwrap();
    log::set_max_level(log::LevelFilter::Info);
    log::info!("my PID is {}", xous::process::id());

    let xns = xous_names::XousNames::new().unwrap();
    let sid = xns.register_name(BOOK_SERVER_NAME, None).expect("can't register server");
    log::trace!("registered with NS -- {:?}", sid);

    // create the book object
    let mut book = Book::new(sid);

    // this is the main event loop for the app.
    let mut allow_redraw = true;
    let mut into_allow_redraw = false;
    loop {
        let msg = xous::receive_message(sid).unwrap();
        match FromPrimitive::from_usize(msg.body.id()) {
            Some(AppOp::Redraw) => {
                log::info!("got AppOp::Redraw");
                if allow_redraw {
                    book.focus();
                    book.update();
                }
            }
            Some(AppOp::Rawkeys) => xous::msg_scalar_unpack!(msg, k1, k2, k3, k4, {
                log::info!("got AppOp::Rawkeys");
                let keys = [
                    core::char::from_u32(k1 as u32).unwrap_or('\u{0000}'),
                    core::char::from_u32(k2 as u32).unwrap_or('\u{0000}'),
                    core::char::from_u32(k3 as u32).unwrap_or('\u{0000}'),
                    core::char::from_u32(k4 as u32).unwrap_or('\u{0000}'),
                ];
                book.rawkeys(keys);
                log::info!("done with AppOp::Rawkeys");
            }),
            Some(AppOp::FocusChange) => xous::msg_scalar_unpack!(msg, new_state_code, _, _, _, {
                log::info!("got AppOp::FocusChange");
                let new_state = gam::FocusState::convert_focus_change(new_state_code);
                log::info!("focus change: {:?}", new_state);
                match new_state {
                    gam::FocusState::Background => {
                    }
                    gam::FocusState::Foreground => {
                    }
                }
            }),
            Some(AppOp::Quit) => xous::msg_blocking_scalar_unpack!(msg, _, _, _, _, {
                log::info!("got AppOp::Quit");
                break;
            }),
            _ => log::error!("couldn't convert opcode: {:?}", msg)
        }
    }
    // clean up our program
    log::trace!("main loop exit, destroying servers");
    xns.unregister_server(sid).unwrap();
    xous::destroy_server(sid).unwrap();
    log::trace!("quitting");
    xous::terminate_process(0)
}
