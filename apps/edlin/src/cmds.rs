use xous::{MessageEnvelope};
use xous_ipc::String;
use core::fmt::Write;

use std::collections::HashMap;
/////////////////////////// Common items to all commands
pub trait ShellCmdApi<'a> {
    // user implemented:
    // called to process the command with the remainder of the string attached
    fn process(&mut self, args: String::<1024>, env: &mut CommonEnv) -> Result<Option<String::<1024>>, xous::Error>;
    // called to process incoming messages that may have been origniated by the most recently issued command
    fn callback(&mut self, msg: &MessageEnvelope, _env: &mut CommonEnv) -> Result<Option<String::<1024>>, xous::Error> {
        log::info!("received unhandled message {:?}", msg);
        Ok(None)
    }

    // created with cmd_api! macro
    // checks if the command matches the current verb in question
    fn matches(&self, verb: &str) -> bool;
    // returns my verb
    fn verb(&self) -> &'static str;
}
// the argument to this macro is the command verb
macro_rules! cmd_api {
    ($verb:expr) => {
        fn verb(&self) -> &'static str {
            stringify!($verb)
        }
        fn matches(&self, verb: &str) -> bool {
            if verb == stringify!($verb) {
                true
            } else {
                false
            }
        }
    };
}

use trng::*;
/////////////////////////// Command shell integration
#[derive(Debug)]
#[allow(dead_code)] // there's more in the envornment right now than we need for the demo
pub struct CommonEnv {
    llio: llio::Llio,
    com: com::Com,
    codec: codec::Codec,
    ticktimer: ticktimer_server::Ticktimer,
    gam: gam::Gam,
    cb_registrations: HashMap::<u32, String::<256>>,
    trng: Trng,
    xns: xous_names::XousNames,
}
impl CommonEnv {
    pub fn register_handler(&mut self, verb: String::<256>) -> u32 {
        let mut key: u32;
        loop {
            key = self.trng.get_u32().unwrap();
            // reserve the bottom 1000 IDs for the main loop enums.
            if !self.cb_registrations.contains_key(&key) && (key > 1000) {
                break;
            }
        }
        self.cb_registrations.insert(key, verb);
        key
    }
}

/*
    To add a new command:
        0. ensure that the command implements the ShellCmdApi (above)
        1. mod/use the new command
        2. create an entry for the command's storage in the CmdEnv structure
        3. initialize the persistant storage here
        4. add it to the "commands" array in the dispatch() routine below

    Side note: if your command doesn't require persistent storage, you could,
    technically, generate the command dynamically every time it's called.
*/

///// 1. add your module here, and pull its namespace into the local crate
mod audio;     use audio::*;


enum EdlinMode {
    Inserting,
    Command
}

pub struct Edlin {
    data:Vec<std::string::String>,
    //data:Vec<String<512>>,
    mode:EdlinMode,
    line_cursor: usize
}

impl Edlin {
    pub fn process(&mut self, line:&std::string::String) -> Vec<std::string::String> {

        match self.mode {
            EdlinMode::Inserting => {
                if line.trim().eq(".") {
                    self.mode = EdlinMode::Command;
                    return Vec::new();
                } else {
                    self.data.insert(self.line_cursor, std::string::String::from(line));  // TODO Insert line at current line_cursor
                    self.line_cursor += 1;
                }
            }
            EdlinMode::Command => {
                if line.to_lowercase().starts_with("i") || line.to_lowercase().ends_with("i") {
                    self.mode = EdlinMode::Inserting;
                    if !line.to_lowercase().starts_with("i") {
                        let digits: Vec<&str> = line.matches(char::is_numeric).collect();
                        let mut line_to_insert_before = digits.join("").parse::<usize>().unwrap();
                        if line_to_insert_before >= self.data.len() {
                            line_to_insert_before = self.data.len()
                        }
                        self.line_cursor = line_to_insert_before - 1
                    }
                }
                if line.to_lowercase().ends_with("d") {
                    let mut del_start = self.line_cursor-1;
                    let mut del_cease = self.line_cursor;
                    let mut without_d = line.to_lowercase().replace("d", "");
                    if without_d.contains(",") {
                        let pair: Vec<&str> = without_d.split(',').collect();
                        del_start = pair[0].parse::<usize>().unwrap();
                        del_cease = pair[1].parse::<usize>().unwrap() + 1;
                    } else if without_d.len() > 0 {
                        del_start = without_d.parse::<usize>().unwrap();
                        del_cease = without_d.parse::<usize>().unwrap() + 1;
                    }
                    if del_cease > self.data.len() {
                        del_cease = self.data.len();
                    }
                    if del_start >= del_cease {
                        del_start = del_cease - 1;
                    }
                    println!("Deleting {} to {}", del_start, del_cease);
                    for i in (del_start..del_cease).rev() {
                        self.data.remove(i);
                        if self.line_cursor > self.data.len() {
                            self.line_cursor = self.data.len()
                        }
                    }

                }
                if line.contains("p") || line.contains("P") {
                    return self.data.clone()
                }
                if line.contains("l") || line.contains("L") {
                    let mut result: Vec<std::string::String> = Vec::new();
                    for (i, line) in self.data.iter().enumerate() {
                        result.insert(i, format!("{}: {}", i, line));
                    }
                    return result;
                }
            }
        }
        return Vec::new();
    }
}





pub struct CmdEnv {
    common_env: CommonEnv,
    lastverb: String::<256>,
    ///// 2. declare storage for your command here.
    audio_cmd: Audio,
    edlin: Edlin,
}
impl CmdEnv {
    pub fn new(xns: &xous_names::XousNames) -> CmdEnv {
        let ticktimer = ticktimer_server::Ticktimer::new().expect("Couldn't connect to Ticktimer");
        log::info!("creating CommonEnv");
        let common = CommonEnv {
            llio: llio::Llio::new(&xns),
            com: com::Com::new(&xns).expect("could't connect to COM"),
            codec: codec::Codec::new(&xns).expect("couldn't connect to CODEC"),
            ticktimer,
            gam: gam::Gam::new(&xns).expect("couldn't connect to GAM"),
            cb_registrations: HashMap::new(),
            trng: Trng::new(&xns).unwrap(),
            xns: xous_names::XousNames::new().unwrap(),
        };

        let mut edlin = Edlin {
            data: Vec::new(),
            mode: EdlinMode::Command,
            line_cursor: 0
        };
        edlin.data.push(std::string::String::from("Hello world."));
        edlin.data.push(std::string::String::from("This is a test."));
        edlin.line_cursor = 2;



        log::info!("done creating CommonEnv");
        CmdEnv {
            common_env: common,
            lastverb: String::<256>::new(),
            ///// 3. initialize your storage, by calling new()
            audio_cmd: Audio::new(&xns),
            edlin: edlin,
        }
    }

    pub fn dispatch(&mut self, maybe_cmdline: Option<&mut String::<1024>>, maybe_callback: Option<&MessageEnvelope>) -> Result<Option<String::<1024>>, xous::Error> {
        let mut ret = String::<1024>::new();

        let commands: &mut [& mut dyn ShellCmdApi] = &mut [
            ///// 4. add your command to this array, so that it can be looked up and dispatched
            &mut self.audio_cmd,
        ];

        if let Some(cmdline) = maybe_cmdline {

            match self.edlin.mode {
                EdlinMode::Command => {
                    write!(ret, "");
                }
                EdlinMode::Inserting => {
                    write!(ret, " {}: ", self.edlin.line_cursor);
                }
            }
            let line = std::string::String::from(cmdline.as_str().unwrap());
            let result = self.edlin.process(&line);
            //let result = self.edlin.process(&std::string::String::from(line.trim()));
            for result_line in result {
                write!(ret, "{}\n", result_line);
            }


            Ok(Some(ret))


            //let maybe_verb = tokenize(cmdline);

            //let mut cmd_ret: Result<Option<String::<1024>>, xous::Error> = Ok(None);
            //if let Some(verb_string) = maybe_verb {
            //    let verb = verb_string.to_str();

            //    // search through the list of commands linearly until one matches,
            //    // then run it.
            //    let mut match_found = false;
            //    for cmd in commands.iter_mut() {
            //        if cmd.matches(verb) {
            //            match_found = true;
            //            cmd_ret = cmd.process(*cmdline, &mut self.common_env);
            //            self.lastverb.clear();
            //            write!(self.lastverb, "{}", verb).expect("couldn't record last verb");
            //        };
            //    }

            //    // if none match, create a list of available commands
            //    if !match_found {
            //        let mut first = true;
            //        write!(ret, "Commands: ").unwrap();
            //        for cmd in commands.iter() {
            //            if !first {
            //                ret.append(", ")?;
            //            }
            //            ret.append(cmd.verb())?;
            //            first = false;
            //        }
            //        Ok(Some(ret))
            //    } else {
            //        cmd_ret
            //    }
            //} else {
            //    Ok(None)
            //}
        } else if let Some(callback) = maybe_callback {
            let mut cmd_ret: Result<Option<String::<1024>>, xous::Error> = Ok(None);
            // first check and see if we have a callback registration; if not, just map to the last verb
            let verb = match self.common_env.cb_registrations.get(&(callback.body.id() as u32)) {
                Some(verb) => {
                    verb.to_str()
                },
                None => {
                    self.lastverb.to_str()
                }
            };
            // now dispatch
            let mut verbfound = false;
            for cmd in commands.iter_mut() {
                if cmd.matches(verb) {
                    cmd_ret = cmd.callback(callback, &mut self.common_env);
                    verbfound = true;
                    break;
                };
            }
            if verbfound {
                cmd_ret
            } else {
                Ok(None)
            }
        } else {
            Ok(None)
        }
    }
}

/// extract the first token, as delimited by spaces
/// modifies the incoming line by removing the token and returning the remainder
/// returns the found token
pub fn tokenize(line: &mut String::<1024>) -> Option<String::<1024>> {
    let mut token = String::<1024>::new();
    let mut retline = String::<1024>::new();

    let lineiter = line.as_str().unwrap().chars();
    let mut foundspace = false;
    let mut foundrest = false;
    for ch in lineiter {
        if ch != ' ' && !foundspace {
            token.push(ch).unwrap();
        } else if foundspace && foundrest {
            retline.push(ch).unwrap();
        } else if foundspace && ch != ' ' {
            // handle case of multiple spaces in a row
            foundrest = true;
            retline.push(ch).unwrap();
        } else {
            foundspace = true;
            // consume the space
        }
    }
    line.clear();
    write!(line, "{}", retline.as_str().unwrap()).unwrap();
    if token.len() > 0 {
        Some(token)
    } else {
        None
    }
}
