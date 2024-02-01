use core::fmt::Write;

use xous_ipc::String;

use crate::{CommonEnv, ShellCmdApi};

#[derive(Debug)]
pub struct Vibe {}
impl Vibe {
    pub fn new() -> Self { Vibe {} }
}

impl<'a> ShellCmdApi<'a> for Vibe {
    cmd_api!(vibe);

    // inserts boilerplate for command API

    fn process(
        &mut self,
        args: String<1024>,
        env: &mut CommonEnv,
    ) -> Result<Option<String<1024>>, xous::Error> {
        let mut ret = String::<1024>::new();
        let helpstring = "vibe [on] [off] [long] [double]";

        let mut tokens = args.as_str().unwrap().split(' ');

        if let Some(sub_cmd) = tokens.next() {
            match sub_cmd {
                "on" => {
                    env.gam.set_vibe(true).unwrap();
                    write!(ret, "Keyboard vibrate on").unwrap();
                }
                "off" => {
                    env.gam.set_vibe(false).unwrap();
                    write!(ret, "Keyboard vibrate off").unwrap();
                }
                "long" => {
                    env.llio.vibe(llio::VibePattern::Long).unwrap();
                    write!(ret, "Long vibe").unwrap();
                }
                "double" => {
                    env.llio.vibe(llio::VibePattern::Double).unwrap();
                    write!(ret, "Double vibe").unwrap();
                }
                _ => write!(ret, "{}", helpstring).unwrap(),
            }
        } else {
            write!(ret, "{}", helpstring).unwrap();
        }
        Ok(Some(ret))
    }
}
