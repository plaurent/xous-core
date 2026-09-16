//! A no-op IME predictor.
//!
//! `UxType::Chat` requires *a* predictor server for the text-entry line to
//! function, but we don't want an autocomplete bar — the function keys are used
//! for app actions and the prediction row isn't reachable. This server satisfies
//! the IME protocol while never offering a prediction (all triggers off, every
//! prediction slot invalid), so the prediction strip stays empty.
//!
//! Modeled on `libs/chat/src/icontray.rs`. It runs on its own thread for the life
//! of the app process; unlike icontray it does not `terminate_process` on Quit
//! (that would kill the whole app), it simply keeps serving until the process
//! exits.

use std::thread;

use ime_plugin_api::*;
use num_traits::*;
use xous::msg_scalar_unpack;
use xous_ipc::Buffer;

pub const SERVER_NAME_OLLAMA_PREDICTOR: &str = "_ollama null predictor_";

/// Spawn the predictor server on a background thread.
pub fn start() { let _ = thread::spawn(move || server()); }

fn server() {
    let xns = xous_names::XousNames::new().unwrap();
    let sid = xns.register_name(SERVER_NAME_OLLAMA_PREDICTOR, None).expect("can't register predictor server");

    // No triggers: the IME never asks us to commit on newline/punctuation/space.
    let triggers = PredictionTriggers { newline: false, punctuation: false, whitespace: false };
    let mut api_token: Option<[u32; 4]> = None;

    loop {
        let mut msg = xous::receive_message(sid).unwrap();
        match FromPrimitive::from_usize(msg.body.id()) {
            Some(Opcode::Acquire) => {
                let mut buffer =
                    unsafe { Buffer::from_memory_message_mut(msg.body.memory_message_mut().unwrap()) };
                let mut ret = buffer.to_original::<AcquirePredictor, _>().unwrap();
                if api_token.is_none() {
                    if let Some(token) = ret.token {
                        api_token = Some(token);
                    } else {
                        let new_token = xous::create_server_id().unwrap().to_array();
                        ret.token = Some(new_token);
                        api_token = Some(new_token);
                    }
                } else {
                    ret.token = None;
                }
                buffer.replace(ret).unwrap();
            }
            Some(Opcode::Release) => msg_scalar_unpack!(msg, t0, t1, t2, t3, {
                let token = [t0 as u32, t1 as u32, t2 as u32, t3 as u32];
                if api_token == Some(token) {
                    api_token.take();
                }
            }),
            Some(Opcode::Input) => {}
            Some(Opcode::Picked) => {}
            Some(Opcode::Prediction) => {
                let mut buffer =
                    unsafe { Buffer::from_memory_message_mut(msg.body.memory_message_mut().unwrap()) };
                let mut prediction: Prediction = buffer.to_original::<Prediction, _>().unwrap();
                // never offer a prediction
                prediction.string.clear();
                prediction.valid = false;
                buffer.replace(Return::Prediction(prediction)).expect("couldn't return Prediction");
            }
            Some(Opcode::Unpick) => {}
            Some(Opcode::GetPredictionTriggers) => {
                xous::return_scalar(msg.sender, triggers.into())
                    .expect("couldn't return GetPredictionTriggers");
            }
            // Ignore Quit: the thread dies with the process; terminating here would
            // take the whole app down.
            Some(Opcode::Quit) => {}
            None => log::error!("ollama predictor: unknown Opcode"),
        }
    }
}
