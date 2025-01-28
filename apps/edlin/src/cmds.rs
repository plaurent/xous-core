use xous::{MessageEnvelope};
use xous_ipc::String;
use core::fmt::Write;
use std::fs::File;
use std::io::{Write as StdWrite, Error};
use std::path::PathBuf;
use std::io::{Read, ErrorKind};
use std::time::{Duration, Instant};
use std::net::{IpAddr, TcpStream, TcpListener};

const ACCEPT: &str = "Accept";
const ACCEPT_JSON: &str = "application/json";
const ACCEPT_TEXTHTML: &str = "text/html";

use ureq;

use retrobasic;



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
//mod audio;     use audio::*;


enum EdlinMode {
    Inserting,
    Command,
    Editing
}

pub struct Edlin {
    data:Vec<std::string::String>,
    //data:Vec<String<512>>,
    mode:EdlinMode,
    line_cursor: usize,
    current_backlight_setting: u8,
    gam: gam::Gam,
    com: com::Com,
}

impl Edlin {


    fn is_string_numeric(&mut self, str: &std::string::String) -> bool {
        for c in str.chars() {
            if !c.is_numeric() {
                return false;
            }
        }
        return true;
    }


    fn ls(&mut self) -> Vec<std::string::String> {
        let mut result: Vec<std::string::String> = Vec::new();
        const EDLIN_DICT: &str = "edlin";
        let mut keypath = PathBuf::new();
        keypath.push(EDLIN_DICT);

        for dir in std::fs::read_dir(&keypath) {
            for entry in dir {
                let path0 = entry.unwrap().path();
                let path = path0.to_str().unwrap();
                log::info!("path '{}'", path);
                if path.ends_with("_line0") {
                    log::info!("LINE0 path '{}'", path);
                    // TODO use system path separator
                    let row = format!("{}", std::string::String::from(path).replacen("edlin:", "", 1).replacen("edlin/", "", 1).replace("_line0", ""));
                    result.push(row);
                }
            }
        }
        return result;
    }


    pub fn post_string(&mut self, url: &str, request_body: &str) -> Result<ureq::Response, ureq::Error> {
    ureq::post(&url)
        .set(ACCEPT, ACCEPT_JSON)
        .send_string(request_body)
    }

    pub fn post_json(&mut self, url: &str, data: &str) -> Result<ureq::Response, ureq::Error> {
    ureq::post(&url)
        .set(ACCEPT, ACCEPT_JSON)
        .send_json(ureq::json!({
            "data": data
        }))
    }

    pub fn get_json(url: &str) -> Result<ureq::Response, ureq::Error> {
    ureq::get(&url)
        .set(ACCEPT, ACCEPT_JSON)
        .call()
    }

    pub fn get_texthtml(&mut self, url: &str) -> Result<ureq::Response, ureq::Error> {
    ureq::get(&url)
        .set(ACCEPT, ACCEPT_TEXTHTML)
        .call()
    }

    pub fn geturl(&mut self, url:&str) -> Option<std::string::String> {
        let response = self.get_texthtml(url);
        match response {
            Ok(response) => {
                if let Ok(body) = response.into_string() {
                    Some(body)
                } else {
                    Some("Error: could not convert response into String".to_string())
                    //None
                }
            },
            Err(ureq::Error::Status(_code, response)) => {
                /* the server returned an unexpected status
                code (such as 400, 500 etc) */
                let err_body = response.into_string().unwrap();
                Some(err_body.to_string())
                //log::info!("ERROR code {} err_body = {}", code, err_body);
                //None
            }
            Err(e) => {
                Some(e.to_string())
                //log::info!("ERROR in handle_response: {:?}", e);
                //None
            }

        }
        //return self.get_texthtml(url).unwrap().into_string().unwrap();
    }

    fn rm(&mut self, filename: std::string::String) -> Result<(), Error> {
        const EDLIN_DICT: &str = "edlin";
        let mut keypath = PathBuf::new();
        keypath.push(EDLIN_DICT);

        for dir in std::fs::read_dir(&keypath) {
            for entry in dir {
                let path0 = entry.unwrap().path();
                let path = path0.to_str().unwrap();
                log::info!("path '{}'", path);
                // TODO use system path separator
                let needstartwith1 = format!("edlin/{}_", filename);
                let needstartwith2 = format!("edlin:{}_", filename);
                if path.starts_with(needstartwith1.as_str()) || path.starts_with(needstartwith2.as_str()) {
                    //log::info!("WOULD DELETE '{}'", path);
                    std::fs::remove_file(&path0);
                    //let row = format!("{}", std::string::String::from(path).replace("edlin/", "").replace("_line0", ""));
                    //result.push(row);
                } else {
                    //log::info!("not deleting '{}'", path);
                }
            }
        }
        Ok(())
    }

    fn load(&mut self, filename: std::string::String) -> Result<(), Error> {
        self.data.clear();
        const EDLIN_DICT: &str = "edlin";
        let mut keypath = PathBuf::new();
        keypath.push(EDLIN_DICT);
        if std::fs::metadata(&keypath).is_ok() { // keypath exists


            self.line_cursor = 0;

            loop {
                let key = format!("{}_line{}", filename, self.line_cursor);
                let mut keypathline = keypath.clone();
                keypathline.push(key);


                if let Ok(mut file)= File::open(keypathline) {
                    let mut value = std::string::String::new();
                    file.read_to_string(&mut value)?;

                    if self.line_cursor >= self.data.len() {
                        self.line_cursor = self.data.len()
                    }
                    self.data.insert(self.line_cursor, std::string::String::from(value.as_str()));
                    self.line_cursor += 1;
                    log::info!("loaded lin '{}'", value.as_str());
                } else {
                    break;
                }
                log::info!("Loaded {} lines from files.", self.data.len());
            }



        } else {
            log::info!("dict '{}' does NOT exist.. nothing has been saved", EDLIN_DICT);
        }

        Ok(())

    }

    fn save(&mut self, filename: std::string::String) -> Result<(), Error> {
            const EDLIN_DICT: &str = "edlin";
            let mut keypath = PathBuf::new();
            keypath.push(EDLIN_DICT);
            if std::fs::metadata(&keypath).is_ok() { // keypath exists
                // log::info!("dict '{}' exists", MTXCLI_DICT);
            } else {
                log::info!("dict '{}' does NOT exist.. creating it", EDLIN_DICT);
                std::fs::create_dir_all(&keypath)?;
            }


            for (i, line) in self.data.iter().enumerate() {
                //log::info!("writing line '{}' {} ", i, line);
                let key = format!("{}_line{}", filename, i);
                let mut keypathline = keypath.clone();
                keypathline.push(key);
                File::create(keypathline)?.write_all(line.as_bytes())?;
            }


            Ok(())
    }

    pub fn process(&mut self, line:&std::string::String) -> Vec<std::string::String> {

        match self.mode {
            EdlinMode::Inserting => {
                if line.trim().eq(".") {
                    self.mode = EdlinMode::Command;
                    return vec![format!(".")];
                } else {
                    if self.line_cursor >= self.data.len() {
                        self.line_cursor = self.data.len()
                    }
                    self.data.insert(self.line_cursor, std::string::String::from(line));
                    let result = format!("*{}: {}", self.line_cursor, line);
                    //let result = format!("{}", line);
                    self.line_cursor += 1;
                    return vec![result];
                }
            }
            EdlinMode::Editing => {
                self.data.remove(self.line_cursor);
                self.data.insert(self.line_cursor, std::string::String::from(line));
                self.mode = EdlinMode::Command;
                return vec![format!(".")];
            }
            EdlinMode::Command => {
                if line.len() > 0 && self.is_string_numeric(line) {
                    self.mode = EdlinMode::Editing;
                    self.line_cursor = line.parse::<usize>().unwrap();
                    if self.data.get(self.line_cursor).unwrap().len() > 127 {
                        self.mode = EdlinMode::Command;
                        return vec![std::string::String::from("Line too long to edit. Try # wrapping.")];
                    }
                    match self.gam.type_chars(self.data.get(self.line_cursor).unwrap()) {
                        Ok(_) => {
                            //write!(ret, "Edit the value and press enter:").unwrap()
                        }
                        _ => {
                            //write!(ret, "Couldn't type out write command.").unwrap()
                        }
                    }
                    return vec![format!("?")];
                }
                if line.starts_with("u") {
                    log::info!("--> grabbing {}", line);
                    let url = line.replace("u ", "");
                    let one_long_string = self.geturl(url.as_str()).unwrap();
                    if self.line_cursor >= self.data.len() {
                        self.line_cursor = self.data.len()
                    }
                    self.data.insert(self.line_cursor, std::string::String::from(one_long_string));
                    return vec![std::string::String::from("Grabbed URL.")];
                }
                if line.starts_with("t") {
                    log::info!("--> posting {}", line);
                    let url = line.replace("t ", "");

                    let body= self.data.iter().map(|x| x.to_string()).collect::<Vec<_>>().join("\n");
                    // let body = std::string::String::from("This is a test of data via json");
                    let result = self.post_json(url.as_str(), body.as_str()).expect("Post didn't work");
                    log::info!("--> posted {}", line);
                    let resultString = std::string::String::from(result.into_string().unwrap());
                    log::info!("result was {}", resultString);
                    return vec![resultString];
                }
                if line.starts_with("b") {  // set brightness
                    let digits: Vec<&str> = line.matches(char::is_numeric).collect();
                    let number = digits.join("").parse::<u8>().unwrap();
                    self.current_backlight_setting = number;
                    self.com.set_backlight(self.current_backlight_setting, self.current_backlight_setting).unwrap();
                    return vec![format!("Brightness set to {}/255.", self.current_backlight_setting)];
                }
                if line.starts_with("z") {  // run BASIC
                    let mut one_long_string = self.data.iter().map(|x| x.to_string()).collect::<Vec<_>>().join("\n");
                    one_long_string.push_str("\n");
                    let result = retrobasic::run_prog(one_long_string);
                    return vec![format!("result {}.", result)];
                }

                if line.ends_with("#") {
                    let mut LEN_FOR_WRAP = 35;
                    if !line.starts_with("#") {
                        let digits: Vec<&str> = line.matches(char::is_numeric).collect();
                        let mut number = digits.join("").parse::<usize>().unwrap();
                        LEN_FOR_WRAP = number;
                    }
                    let one_long_string = self.data.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(" ");
                    let remove_dup_spaces = one_long_string.replace("  ", " ");
                    let words = remove_dup_spaces.split(" ");
                    self.data.clear();
                    let mut line = std::string::String::new();
                    for word in words {
                        line.push_str(format!("{} ", word).as_str());
                        //log::info!("Adding line '{}' len is {} ", line, line.len());
                        if line.len() > LEN_FOR_WRAP {
                            self.data.push(line.clone());
                            line = std::string::String::from("");
                        }
                    }
                    if line.len() > 0 {
                        self.data.push(line.clone());
                    }
                    return vec![format!("Wrapped to {} lines.", self.data.len())];
                }
                if line.to_lowercase().starts_with("w") {
                    let filename = line.replacen("w ", "", 1).replacen("W ", "", 1);
                    if filename.len() > 0 {
                        self.save(filename);
                        return vec![format!("*{}:", self.line_cursor)];
                    } else {
                        return vec![std::string::String::from("Please enter a filename after w.")];
                    }
                }
                if line.to_lowercase().starts_with("r"){
                    let filename = line.replacen("r ", "", 1).replacen("R ", "", 1);
                    if filename.len() > 0 {
                        self.load(filename);
                        return vec![format!("*{}:", self.line_cursor)];
                    } else {
                        return vec![std::string::String::from("Please enter a filename after r.")];
                    }
                }
                if line.to_lowercase().starts_with("x"){
                    let filename = line.replacen("x ", "", 1).replacen("X ", "", 1);
                    if filename.len() > 0 {
                        self.rm(filename);
                        return vec![std::string::String::from("Ok.")];
                    } else {
                        return vec![std::string::String::from("Please enter a filename after x.")];
                    }
                }
                if line.to_lowercase().starts_with("?"){
                    //return vec![std::string::String::from("Edlin help.\ni insert\nd delete\nw write\nr read\n* list files\nx delete file\nnumber edit/select line\nl list all\np print\nn next n lines\n[num]# wrap text\nu get http url\nb [num] set brightness")];
                    return vec![format!("Edlin help. {} lines.\ni insert\nd delete\nw write\nr read\n* list files\nx delete file\nnumber edit/select line\nl list all\np print\nn next n lines\n[num]# wrap text\nu get http url\nb [num] set brightness", self.data.len())];
                }
                if line.to_lowercase().starts_with("i") || line.to_lowercase().ends_with("i") {
                    self.mode = EdlinMode::Inserting;
                    if !line.to_lowercase().starts_with("i") {
                        let digits: Vec<&str> = line.matches(char::is_numeric).collect();
                        let mut line_to_insert_before = digits.join("").parse::<usize>().unwrap();
                        if line_to_insert_before >= self.data.len() {
                            line_to_insert_before = self.data.len()
                        }
                        self.line_cursor = line_to_insert_before;
                    }
                    return vec![format!("*{}:", self.line_cursor)];
                }
                if line.to_lowercase().ends_with("d") {
                    let mut del_start = self.line_cursor;
                    let mut del_cease = self.line_cursor;
                    let without_d = line.to_lowercase().replace("d", "");
                    if without_d.contains(",") {
                        let pair: Vec<&str> = without_d.split(',').collect();
                        del_start = pair[0].parse::<usize>().unwrap();
                        del_cease = pair[1].parse::<usize>().unwrap();
                    } else if without_d.len() > 0 {
                        del_start = without_d.parse::<usize>().unwrap();
                        del_cease = without_d.parse::<usize>().unwrap();
                    }
                    if del_cease > self.data.len()-1 {
                        del_cease = self.data.len()-1;
                    }
                    if del_start > del_cease {
                        del_start = del_cease;
                    }
                    if del_start <= self.data.len() - 1 && del_cease <= self.data.len() {
                        println!("Deleting {} to {}", del_start, del_cease);
                        if del_start == del_cease {
                            self.data.remove(del_start);
                            if self.line_cursor > self.data.len() {
                                self.line_cursor = self.data.len()
                            }
                        }
                        for i in (del_start..del_cease).rev() {
                            self.data.remove(i);
                            if self.line_cursor > self.data.len() {
                                self.line_cursor = self.data.len()
                            }
                        }
                        return vec![format!("Deleted {} to {}", del_start, del_cease)];
                    } else {
                        return vec![format!("Can't delete beyond {}", self.data.len()-1)];
                    }
                }
                if line.contains("v") || line.contains("v") {
                    return self.data.clone()
                }
                if line.contains("*") {
                    return self.ls();
                }
                if line.contains("l") || line.contains("L") {
                    let mut result: Vec<std::string::String> = Vec::new();
                    for (i, line) in self.data.iter().enumerate() {
                        if i == self.line_cursor {
                            result.insert(i, format!("*{}: {}", i, line));
                        } else {
                            result.insert(i, format!(" {}: {}", i, line));
                        }
                    }
                    return result;
                }
                if line.contains("n") || line.contains("N") {
                    if !line.to_lowercase().starts_with("n") {
                        let digits: Vec<&str> = line.matches(char::is_numeric).collect();
                        let line_to_next_from = digits.join("").parse::<usize>().unwrap();
                        self.line_cursor = line_to_next_from;
                    }
                    let NUM_LINES_PER_PAGE = 5;
                    let mut result: Vec<std::string::String> = Vec::new();
                    let mut upto = self.line_cursor + NUM_LINES_PER_PAGE;
                    if upto > self.data.len()  {
                        upto = self.data.len();
                    }
                    for (i, line) in self.data[self.line_cursor..upto].iter().enumerate() {
                        result.insert(i, format!("{}: {}", self.line_cursor+i, line));
                    }
                    self.line_cursor = self.line_cursor + NUM_LINES_PER_PAGE;
                    if self.line_cursor > self.data.len()-1 {
                        self.line_cursor = self.data.len()-1;
                    }
                    return result;
                }
                if line.contains("p") || line.contains("P") || line.eq("") {
                    // NOTE: Duplication of some code for "n" except no line numbers are printed
                    // and all lines are concatenated.
                    // TODO remove duplication
                    if !line.to_lowercase().starts_with("p") && !line.eq("") {
                        let digits: Vec<&str> = line.matches(char::is_numeric).collect();
                        let line_to_next_from = digits.join("").parse::<usize>().unwrap();
                        self.line_cursor = line_to_next_from;
                    }
                    let NUM_LINES_PER_PAGE = 5;
                    let mut result: Vec<std::string::String> = Vec::new();
                    let mut upto = self.line_cursor + NUM_LINES_PER_PAGE;
                    if upto > self.data.len()  {
                        upto = self.data.len();
                    }
                    if self.line_cursor > self.data.len() {
                        self.line_cursor = self.data.len();
                    }
                    for (i, line) in self.data[self.line_cursor..upto].iter().enumerate() {
                        result.insert(i, format!("{}", line));
                    }

                    let one_long_string = result.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(" ");
                    let remove_dup_spaces = one_long_string.replace("  ", " ");

                    self.line_cursor = self.line_cursor + NUM_LINES_PER_PAGE;
                    if self.line_cursor > self.data.len()-1 {
                        self.line_cursor = self.data.len()-1;
                    }
                    return vec!(remove_dup_spaces);
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
    //audio_cmd: Audio,
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

        let edlin = Edlin {
            data: Vec::new(),
            mode: EdlinMode::Command,
            line_cursor: 0,
            current_backlight_setting: 254,
            gam: gam::Gam::new(&xns).expect("couldn't connect to GAM"),
            com: com::Com::new(&xns).unwrap(),
        };
        //edlin.data.push(std::string::String::from("Hello world."));
        //edlin.data.push(std::string::String::from("This is a test."));
        //edlin.line_cursor = 2;



        log::info!("done creating CommonEnv");
        CmdEnv {
            common_env: common,
            lastverb: String::<256>::new(),
            ///// 3. initialize your storage, by calling new()
            //audio_cmd: Audio::new(&xns),
            edlin: edlin,
        }
    }

    pub fn dispatch(&mut self, maybe_cmdline: Option<&mut String::<1024>>, maybe_callback: Option<&MessageEnvelope>) -> Result<Option<String::<1024>>, xous::Error> {
        let mut ret = String::<1024>::new();

        let commands: &mut [& mut dyn ShellCmdApi] = &mut [
            ///// 4. add your command to this array, so that it can be looked up and dispatched
            //&mut self.audio_cmd,
        ];

        if let Some(cmdline) = maybe_cmdline {

            match self.edlin.mode {
                EdlinMode::Command => {
                }
                EdlinMode::Editing => {
                }
                EdlinMode::Inserting => {
                }
            }
            let line = std::string::String::from(cmdline.as_str().unwrap());
            self.edlin.com.set_backlight(self.edlin.current_backlight_setting, self.edlin.current_backlight_setting).unwrap();

            let result = self.edlin.process(&line);
            //let result = self.edlin.process(&std::string::String::from(line.trim()));

            //for result_line in result {
            for (i, result_line) in result.iter().enumerate() {  // self.data.iter().enumerate() {
                if i < result.len()-1 {
                    write!(ret, "{}\n", result_line);
                } else {
                    write!(ret, "{}", result_line);
                }
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
