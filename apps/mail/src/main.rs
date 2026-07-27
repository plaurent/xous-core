#![cfg_attr(target_os = "none", no_std)]
#![cfg_attr(target_os = "none", no_main)]

mod api;
mod mailapp;

use api::*;
use chat::{Chat, Event};
use gam::{MenuItem, MenuPayload};
use mailapp::MailApp;
use num_traits::*;
use xous_ipc::Buffer;

fn main() -> ! {
    // The IMAP path buffers whole messages (headers + body + any inline
    // parts) in RAM while parsing, so give the app a generous stack, same
    // as the other chat-lib apps.
    let stack_size = 1024 * 1024;
    std::thread::Builder::new().stack_size(stack_size).spawn(wrapped_main).unwrap().join().unwrap()
}

fn wrapped_main() -> ! {
    log_server::init_wait().unwrap();
    log::set_max_level(log::LevelFilter::Info);
    log::info!("my PID is {}", xous::process::id());

    // Raise the heap ceiling: a full FETCH of a large message (with
    // base64/quoted-printable parts) can transiently allocate well past the
    // default limit. Mirrors apps/chat-test and the sigchat reference.
    const HEAP_LARGER_LIMIT: usize = 2048 * 1024;
    let new_limit = HEAP_LARGER_LIMIT;
    let result =
        xous::rsyscall(xous::SysCall::AdjustProcessLimit(xous::Limits::HeapMaximum as usize, 0, new_limit));
    if let Ok(xous::Result::Scalar2(1, current_limit)) = result {
        xous::rsyscall(xous::SysCall::AdjustProcessLimit(
            xous::Limits::HeapMaximum as usize,
            current_limit,
            new_limit,
        ))
        .unwrap();
        log::info!("Heap limit increased to: {}", new_limit);
    } else {
        panic!("Unsupported syscall!");
    }

    let xns = xous_names::XousNames::new().unwrap();
    let sid = xns.register_name(SERVER_NAME_MAIL, None).expect("can't register server");
    log::trace!("registered with NS -- {:?}", sid);

    // Stand up the Chat UI. It owns the GAM canvas, draws the scrolling
    // message view + status line, and dispatches F1/F2/F3 (and Focus,
    // menu, post) back to us on `sid` as MailOp messages.
    // NOTE: the generated const is APP_NAME_MAILAPP (derived from the
    // manifest/package key "mailapp"), but its *value* is the context name
    // "mail" — that's the name the GAM shows and the user sees. See the
    // header comment in Cargo.toml for why the crate is "mailapp".
    let chat = Chat::new(
        gam::APP_NAME_MAILAPP,
        gam::APP_MENU_0_MAILAPP,
        Some(xous::connect(sid).unwrap()),
        Some(MailOp::Post as usize),
        Some(MailOp::Event as usize),
        Some(MailOp::Rawkeys as usize),
    );

    let cid = xous::connect(sid).unwrap();
    chat.menu_add(MenuItem {
        name: String::from("Close"),
        action_conn: Some(cid),
        action_opcode: MailOp::Menu as u32,
        action_payload: MenuPayload::Scalar([MenuOp::Noop as u32, 0, 0, 0]),
        close_on_select: true,
    })
    .expect("failed to add menu item");

    let mut app = MailApp::new(&xns);
    let mut first_focus = true;

    loop {
        let msg = xous::receive_message(sid).unwrap();
        log::debug!("got message {:?}", msg);
        match FromPrimitive::from_usize(msg.body.id()) {
            Some(MailOp::Event) => {
                xous::msg_scalar_unpack!(msg, event_code, _, _, _, {
                    match FromPrimitive::from_usize(event_code) {
                        Some(Event::Focus) => {
                            if first_focus {
                                first_focus = false;
                                // Bind a Dialogue so the Chat UI has somewhere
                                // to store/render the posts we push when a
                                // message is opened. Backed by the pddb and
                                // created on first use; done here (not before
                                // the loop) so the pddb is mounted by the time
                                // we touch it, mirroring the sigchat ordering.
                                chat.dialogue_set(mailapp::MAIL_DICT, Some(mailapp::MAIL_DIALOGUE_KEY))
                                    .expect("failed to set dialogue");
                                app.greet(&chat);
                            }
                            chat.redraw();
                        }
                        // F1 = inbox: list recent subjects/senders, open a selection.
                        Some(Event::F1) => app.inbox(&chat),
                        // F2 = compose a new message.
                        Some(Event::F2) => app.compose(&chat),
                        // F3 = mail account settings (server/user/password).
                        Some(Event::F3) => app.settings(&chat),
                        // F4 = reply to the message currently open under F1.
                        Some(Event::F4) => app.reply(&chat),
                        _ => (),
                    }
                });
            }
            Some(MailOp::Menu) => {
                xous::msg_scalar_unpack!(msg, menu_code, _, _, _, {
                    match FromPrimitive::from_usize(menu_code) {
                        Some(MenuOp::Noop) => {}
                        _ => (),
                    }
                });
            }
            Some(MailOp::Post) => {
                // The app doesn't use the free-text input box (compose is a
                // modal form under F2); drain the message so the Chat UI's
                // caller is released.
                let buffer = unsafe { Buffer::from_memory_message(msg.body.memory_message().unwrap()) };
                let _ = buffer.to_original::<String, _>();
            }
            Some(MailOp::Rawkeys) => {}
            Some(MailOp::Quit) => {
                log::info!("got Quit");
                break;
            }
            _ => log::warn!("got unknown message"),
        }
    }
    log::info!("main loop exit, destroying servers");
    xns.unregister_server(sid).unwrap();
    xous::destroy_server(sid).unwrap();
    xous::terminate_process(0)
}
