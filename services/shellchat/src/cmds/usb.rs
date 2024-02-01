use std::fmt::Write;

use usb_device_xous::{UsbDeviceState, UsbDeviceType, UsbHid};

use crate::{CommonEnv, ShellCmdApi};

#[derive(Debug)]
pub struct Usb {
    usb_dev: UsbHid,
}
impl Usb {
    pub fn new() -> Usb { Usb { usb_dev: UsbHid::new() } }
}

impl<'a> ShellCmdApi<'a> for Usb {
    cmd_api!(usb);

    // inserts boilerplate for command API

    fn process(
        &mut self,
        args: xous_ipc::String<1024>,
        _env: &mut CommonEnv,
    ) -> Result<Option<xous_ipc::String<1024>>, xous::Error> {
        let mut ret = xous_ipc::String::<1024>::new();
        #[cfg(not(feature = "mass-storage"))]
        let helpstring = "usb [hid] [fido] [debug] [send <string>] [status] [leds] [lock] [unlock] [kbdtest]";
        #[cfg(feature = "mass-storage")]
        let helpstring = "usb [hid] [fido] [ms] [debug] [send <string>] [status] [leds] [lock] [unlock] [kbdtest] [console] [noconsole]";

        let mut tokens = args.as_str().unwrap().split(' ');

        if let Some(sub_cmd) = tokens.next() {
            match sub_cmd {
                "hid" => {
                    self.usb_dev.ensure_core(usb_device_xous::UsbDeviceType::FidoKbd).unwrap();
                    write!(ret, "USB connected to HID (FIDO + keyboard) core").unwrap();
                }
                #[cfg(feature = "mass-storage")]
                "ms" => {
                    self.usb_dev.ensure_core(usb_device_xous::UsbDeviceType::MassStorage).unwrap();
                    write!(ret, "USB connected to mass storage core").unwrap();
                }
                "fido" => {
                    self.usb_dev.ensure_core(usb_device_xous::UsbDeviceType::Fido).unwrap();
                    write!(ret, "USB connected to FIDO-only core").unwrap();
                }
                "debug" => {
                    self.usb_dev.switch_to_core(usb_device_xous::UsbDeviceType::Debug).unwrap();
                    self.usb_dev.debug_usb(Some(false)).unwrap();
                    write!(ret, "USB connected to Debug core, secrets readable!").unwrap();
                }
                "serial" => {
                    self.usb_dev.ensure_core(usb_device_xous::UsbDeviceType::Serial).unwrap();
                    write!(ret, "USB connected to serial core").unwrap();
                }
                "console" => {
                    self.usb_dev.ensure_core(usb_device_xous::UsbDeviceType::Serial).unwrap();
                    // this will enable input injection mode
                    self.usb_dev.serial_console_input_injection();
                    write!(ret, "USB console connected.").ok();
                }
                "noconsole" => {
                    // this will disable any hooks (including the console input hook)
                    self.usb_dev.serial_clear_input_hooks();
                    write!(ret, "USB console disconnected.").ok();
                }
                "trng" => {
                    self.usb_dev.ensure_core(usb_device_xous::UsbDeviceType::Serial).unwrap();
                    let mode = if let Some(sub_cmd) = tokens.next() {
                        match sub_cmd {
                            "ro" => trng::api::TrngTestMode::Ro,
                            "av" => trng::api::TrngTestMode::Av,
                            "both" => trng::api::TrngTestMode::Both,
                            "cprng" => trng::api::TrngTestMode::Cprng,
                            _ => trng::api::TrngTestMode::Cprng, // this is the "normal" mode of operation
                        }
                    } else {
                        // "normal" mode of operation: Ro + Av fed into CPRNG and then used
                        trng::api::TrngTestMode::Cprng
                    };
                    self.usb_dev.serial_set_trng_mode(mode);
                    write!(ret, "USB TRNG serial sending requested").ok();
                }
                "notrng" => {
                    self.usb_dev.serial_clear_input_hooks();
                    write!(ret, "USB TRNG serial sending should be stopped.").ok();
                }
                "send" => match self.usb_dev.get_current_core() {
                    Ok(UsbDeviceType::FidoKbd) | Ok(UsbDeviceType::Serial) => {
                        let mut val = String::new();
                        join_tokens(&mut val, &mut tokens);
                        match self.usb_dev.send_str(&val) {
                            Ok(n) => write!(ret, "Sent {} chars", n).unwrap(),
                            Err(_e) => write!(ret, "Can't send: are we connected to a host?").unwrap(),
                        }
                    }
                    Ok(UsbDeviceType::Debug) => {
                        write!(ret, "HID core not connected: please issue 'usb hid' first").unwrap();
                    }
                    _ => write!(ret, "Invalid response checking status").unwrap(),
                },
                "kbdtest" => {
                    let mut test_str = String::new();
                    for c in 0x20..0x7F {
                        // includes a space as the first character
                        // safety - the bounds are checked above in the loop to be the printable ASCII
                        // character range.
                        test_str.push(unsafe { char::from_u32_unchecked(c as u32) });
                    }
                    test_str.push('\n');
                    match self.usb_dev.get_current_core() {
                        Ok(UsbDeviceType::FidoKbd) => match self.usb_dev.send_str(&test_str) {
                            Ok(n) => write!(ret, "Sent {} test string", n).unwrap(),
                            Err(_e) => write!(ret, "Can't send: are we connected to a host?").unwrap(),
                        },
                        Ok(UsbDeviceType::Debug) => {
                            write!(ret, "HID core not connected: please issue 'usb hid' first").unwrap();
                        }
                        _ => write!(ret, "Invalid response checking status").unwrap(),
                    }
                }
                "status" => match self.usb_dev.get_current_core() {
                    Ok(UsbDeviceType::Debug) => write!(ret, "Debug core connected").unwrap(),
                    Ok(UsbDeviceType::FidoKbd) => match self.usb_dev.status() {
                        UsbDeviceState::Configured => write!(ret, "HID core connected to host").unwrap(),
                        UsbDeviceState::Suspend => write!(ret, "HID in suspend").unwrap(),
                        _ => write!(ret, "HID not connected to USB host").unwrap(),
                    },
                    #[cfg(feature = "mass-storage")]
                    Ok(UsbDeviceType::MassStorage) => write!(ret, "USB mass storage connected").unwrap(),
                    _ => write!(ret, "Invalid response checking status").unwrap(),
                },
                "leds" => match self.usb_dev.get_current_core() {
                    Ok(UsbDeviceType::FidoKbd) => match self.usb_dev.get_led_state() {
                        Ok(leds) => write!(ret, "LEDs: {:?}", leds).unwrap(),
                        _ => write!(ret, "Not connected to USB host or other error").unwrap(),
                    },
                    Ok(UsbDeviceType::Debug) => {
                        write!(ret, "HID core not connected: please issue 'usb hid' first").unwrap();
                    }
                    _ => write!(ret, "Invalid response checking status").unwrap(),
                },
                "lock" => {
                    self.usb_dev.restrict_debug_access(true).unwrap();
                    write!(ret, "USB debug port locked out; one word at 0x80000000 is disclosable via USB.")
                        .unwrap();
                }
                "unlock" => {
                    self.usb_dev.restrict_debug_access(false).unwrap();
                    write!(ret, "USB debug port unlocked: portions of the device are readable via USB!")
                        .unwrap();
                }
                _ => {
                    write!(ret, "{}", helpstring).unwrap();
                }
            }
        } else {
            write!(ret, "{}", helpstring).unwrap();
        }
        Ok(Some(ret))
    }
}

fn join_tokens<'a>(buf: &mut String, tokens: impl Iterator<Item = &'a str>) {
    for (i, tok) in tokens.enumerate() {
        if i == 0 {
            write!(buf, "{}", tok).unwrap();
        } else {
            write!(buf, " {}", tok).unwrap();
        }
    }
}
