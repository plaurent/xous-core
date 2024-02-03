use super::*;
use gam::*;
use graphics_server::{Gid, Point, Rectangle, TextBounds, TextView, DrawStyle, PixelColor};
use gam::menu::*;
//use gam::menu::api::DrawStyle;
use std::time::{Duration, Instant};

use core::fmt::Write;

use std::net::{IpAddr, TcpStream, TcpListener};
use std::io::{Read, Write as StdWrite, Error, ErrorKind};

use std::fs::File;
//use std::io::{Read, Write as StdWrite, Error, ErrorKind};
use std::path::PathBuf;



use locales::t;


#[derive(PartialEq, Eq)]
enum BookMode {
    Random,
    Tilt
}

const BOOK_RADIUS: i16 = 10;
const MOMENTUM_LIMIT: i32 = 8;
const BORDER_WIDTH: i16 = 5;
const APP_DICTNAME: &str = "book";
const DIM_BACKLIGHT_SETTING: u8 = 20;

pub(crate) struct Book {
    gam: gam::Gam,
    gid: Gid,
    screensize: Point,
    // our security token for making changes to our record on the GAM
    _token: [u32; 4],
    book: Circle,
    momentum: Point,
    trng: trng::Trng,
    modals: modals::Modals,
    mode: BookMode,
    counter: u32,
    com: com::Com,
    current_backlight_setting: u8,
}

impl Book {
    pub(crate) fn new(sid: xous::SID) -> Self {
        let xns = xous_names::XousNames::new().expect("couldn't connect to Xous Namespace Server");
        let gam = gam::Gam::new(&xns).expect("can't connect to Graphical Abstraction Manager");

        let token = gam.register_ux(UxRegistration {
            app_name: xous_ipc::String::<128>::from_str(gam::APP_NAME_BOOK),
            ux_type: gam::UxType::Framebuffer,
            predictor: None,
            listener: sid.to_array(), // note disclosure of our SID to the GAM -- the secret is now shared with the GAM!
            redraw_id: AppOp::Redraw.to_u32().unwrap(),
            gotinput_id: None,
            audioframe_id: None,
            focuschange_id: Some(AppOp::FocusChange.to_u32().unwrap()),
            rawkeys_id: Some(AppOp::Rawkeys.to_u32().unwrap()),
        }).expect("couldn't register Ux context for shellchat");

        let gid = gam.request_content_canvas(token.unwrap()).expect("couldn't get content canvas");
        let screensize = gam.get_canvas_bounds(gid).expect("couldn't get dimensions of content canvas");

        gam.draw_rectangle(gid,
            Rectangle::new_coords_with_style(0, 0, screensize.x, screensize.y,
                DrawStyle::new(PixelColor::Light, PixelColor::Dark, 2))
        ).expect("couldn't draw our rectangle");

        let trng = trng::Trng::new(&xns).unwrap();
        let x = ((trng.get_u32().unwrap() / 2) as i32) % (MOMENTUM_LIMIT * 2) - MOMENTUM_LIMIT;
        let y = ((trng.get_u32().unwrap() / 2) as i32) % (MOMENTUM_LIMIT * 2) - MOMENTUM_LIMIT;

        let mut book = Circle::new(Point::new(screensize.x / 2, screensize.y / 2), BOOK_RADIUS);
        book.style = DrawStyle::new(PixelColor::Dark, PixelColor::Dark, 1);
        gam.draw_circle(gid, book).expect("couldn't erase book's previous position");
        let modals = modals::Modals::new(&xns).unwrap();
        let com = com::Com::new(&xns).unwrap();
        let current_backlight_setting = 100;
        Book {
            gid,
            gam,
            screensize,
            _token: token.unwrap(),
            book,
            momentum: Point::new(x as i16, y as i16),
            trng,
            modals,
            mode: BookMode::Random,
            counter: 0,
            com,
            current_backlight_setting,
        }
    }
    pub(crate) fn update(&mut self) {
        //self.gam.redraw().unwrap();
    }

    pub(crate) fn focus(&mut self) {
        //// draw the background entirely
        //self.gam.draw_rectangle(self.gid,
        //    Rectangle::new_coords_with_style(0, 0, self.screensize.x, self.screensize.y,
        //        DrawStyle::new(PixelColor::Light, PixelColor::Dark, BORDER_WIDTH))
        //).expect("couldn't draw our rectangle");
    }
    pub(crate) fn rawkeys(&mut self, keys: [char; 4]) {
        log::debug!("got rawkey {:?}", keys); // you could use the raw keypresses, but modals are easier...

        //self.com.set_backlight(DIM_BACKLIGHT_SETTING, 0).unwrap();
        //let direction = if keys.iter().as_bytes().contains(8) {
        let direction = if keys.contains(&char::from_u32(8).unwrap()) {
            "prev"
        }
        else if keys.contains(&'-') || keys.contains(&'g') {
            "less"
        }
        else if keys.contains(&'+') || keys.contains(&'h') {
            "more"
        }
        else if keys.contains(&'0') { self.setpddbvalue(&"idx", &"0") ; "list" }
        else if keys.contains(&'1') { self.setpddbvalue(&"idx", &"1") ; "list" }
        else if keys.contains(&'2') { self.setpddbvalue(&"idx", &"2") ; "list" }
        else if keys.contains(&'3') { self.setpddbvalue(&"idx", &"3") ; "list" }
        else if keys.contains(&'4') { self.setpddbvalue(&"idx", &"4") ; "list" }
        else if keys.contains(&'5') { self.setpddbvalue(&"idx", &"5") ; "list" }
        else if keys.contains(&'6') { self.setpddbvalue(&"idx", &"6") ; "list" }
        else if keys.contains(&'7') { self.setpddbvalue(&"idx", &"7") ; "list" }
        else if keys.contains(&'8') { self.setpddbvalue(&"idx", &"8") ; "list" }
        else if keys.contains(&'9') { self.setpddbvalue(&"idx", &"9") ; "list" }
        else if keys.contains(&'!') {
            if self.current_backlight_setting == DIM_BACKLIGHT_SETTING {
                self.current_backlight_setting = 255;
                "next"
            } else {
                self.current_backlight_setting = DIM_BACKLIGHT_SETTING;
                "next"
            }

        } else {
            "next"
        };
        self.com.set_backlight(self.current_backlight_setting, 0).unwrap();

        self.gam.draw_rectangle(self.gid,
            Rectangle::new_with_style(Point::new(0, 0), self.screensize,
            DrawStyle {
                fill_color: Some(PixelColor::Light),
                stroke_color: None,
                stroke_width: 0
            }
        )).expect("can't clear content area");

		let bubble_width = ((self.screensize.x / 10) * 9) as u16;
		let margin = Point::new(4, 4);
		let mut bubble_baseline = self.screensize.y - margin.y;
		//let mut bubble_tv =
		//		TextView::new(self.gid,
		//			TextBounds::GrowableFromBl(
		//				Point::new(margin.x, bubble_baseline),
		//				bubble_width));
		let margin = Point::new(4, 4);
		let mut bubble_tv =
				TextView::new(self.gid,
					TextBounds::CenteredTop(
						Rectangle::new(
							Point::new(margin.x, 0),
							Point::new(self.screensize.x - margin.x, self.screensize.y-margin.y)
						)
					)
		);
		bubble_tv.border_width = 1;
		bubble_tv.draw_border = true;
		bubble_tv.clear_area = true;
		bubble_tv.rounded_border = Some(2);
		bubble_tv.style = GlyphStyle::Large;
		bubble_tv.margin =  Point::new(4, 4);
		bubble_tv.ellipsis = false; bubble_tv.insertion = None;

        self.counter += 1;
        //let mut message = String::from("This is page ");
        //message.push_str(self.counter.to_string().as_str());

		let mut message = self.getpage(direction);
		let mut text = String::from("");
		let mut foundblank = false;
		for line in message.lines() {
			if foundblank { text.push_str(line); }
			if line == "" { foundblank = true; }
		}


		write!(bubble_tv.text, "{}", text).expect("couldn't write history text to TextView");
		self.gam.post_textview(&mut bubble_tv).expect("couldn't render bubble textview");

    }

    pub fn getpddbvalue(&mut self, mykey:&str) -> String {
        let mut keypath = PathBuf::new();
        keypath.push(APP_DICTNAME);
        if std::fs::metadata(&keypath).is_ok() { // keypath exists
        } else {
            log::info!("dict '{}' does NOT exist.. creating it", APP_DICTNAME);
            std::fs::create_dir_all(&keypath);
        }
        keypath.push(mykey);
        let mut result = String::new();
        if let Ok(mut file)= File::open(keypath) {
            file.read_to_string(&mut result);
        } else {
            result = "0".to_string();   // default value
        }
        result
    }

    pub fn setpddbvalue(&mut self, key: &str, value: &str) -> Result<(), Error> {
        log::info!("set '{}' = '{}'", key, value);
        let mut keypath = PathBuf::new();
        keypath.push(APP_DICTNAME);
        if std::fs::metadata(&keypath).is_ok() { // keypath exists
        } else {
            log::info!("dict '{}' does NOT exist.. creating it", APP_DICTNAME);
            std::fs::create_dir_all(&keypath)?;
        }
        keypath.push(key);
        File::create(keypath)?.write_all(value.as_bytes())?;
        Ok(())
    }




    pub(crate) fn getpage(&mut self, direction:&str) -> String {
        let host = self.getpddbvalue(&"url");
        let bookindex = self.getpddbvalue(&"idx");  // which book are we reading, default 0

		let mut path = String::from("book/");
        path.push_str(&direction);
        path.push_str(&"?bookindex=");
        path.push_str(&bookindex);

        log::info!("my Host: {:?}", host);
        log::info!("my Path : {:?}", path);
        //use core::fmt::Write;
		use std::io::Write;

        let mut ret = xous_ipc::String::<1024>::new();

		match TcpStream::connect((host.clone(), 80)) {
			Ok(mut stream) => {
				log::trace!("stream open, setting timeouts");
				stream.set_read_timeout(Some(Duration::from_millis(10_000))).unwrap();
				stream.set_write_timeout(Some(Duration::from_millis(10_000))).unwrap();
				log::debug!("read timeout: {:?}", stream.read_timeout().unwrap().unwrap().as_millis());
				log::debug!("write timeout: {:?}", stream.write_timeout().unwrap().unwrap().as_millis());
				log::info!("my socket: {:?}", stream.local_addr());
				log::info!("peer addr: {:?}", stream.peer_addr());
				log::info!("sending GET request");
				match write!(stream, "GET /{} HTTP/1.1\r\n", path) {
					Ok(_) => log::trace!("sent GET"),
					Err(e) => {
						log::error!("GET err {:?}", e);
						write!(ret, "Error sending GET: {:?}", e).unwrap();
					}
				}
				write!(stream, "Host: {}\r\nAccept: */*\r\nUser-Agent: Precursor/0.9.6\r\n", host).expect("stream error");
				write!(stream, "Connection: close\r\n").expect("stream error");
				write!(stream, "\r\n").expect("stream error");
				log::info!("fetching response....");
				let mut buf = [0u8; 1024];
				match stream.read(&mut buf) {
					Ok(len) => {
						log::trace!("raw response ({}): {:?}", len, &buf[..len]);
						write!(ret, "{}", std::string::String::from_utf8_lossy(&buf[..len.min(buf.len())])).ok(); // let it run off the end
						log::info!("{}NET.TCPGET,{},{}",
							xous::BOOKEND_START,
							std::string::String::from_utf8_lossy(&buf[..len.min(buf.len())]),
							xous::BOOKEND_END);
					}
					Err(e) => write!(ret, "Didn't get response from host: {:?}", e).unwrap(),
				}
			}
			Err(e) => {
				log::info!("error connecting {}", e);
                write!(ret, "Couldn't connect to {}:80: {:?}", host, e).unwrap()
            },
		}
		return ret.to_string();

    }

}


