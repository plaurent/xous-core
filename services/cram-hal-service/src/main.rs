mod api;
use api::*;
#[cfg(feature = "pio")]
mod pio;
use cramium_hal::{
    iox,
    udma::{EventChannel, GlobalConfig, PeriphId},
};
use xous::sender::Sender;

fn try_alloc(ifram_allocs: &mut Vec<Option<Sender>>, size: usize, sender: Sender) -> Option<usize> {
    let mut size_pages = size / 4096;
    if size % 4096 != 0 {
        size_pages += 1;
    }
    log::trace!("try_alloc search for {} pages in alloc vector {:?}", size_pages, ifram_allocs);
    let mut free_start = None;
    let mut found_len = 0;
    for (index, page) in ifram_allocs.iter().enumerate() {
        log::trace!("Checking index {}: {:?}", index, page);
        if page.is_some() {
            log::trace!("Page was allocated, restarting search");
            free_start = None;
            found_len = 0;
            continue;
        } else {
            if free_start.is_some() {
                log::trace!("Adding unallocated page at {} to length", index);
                found_len += 1;
                if found_len >= size_pages {
                    break;
                }
            } else {
                log::trace!("Starting allocation search at {}", index);
                free_start = Some(index);
                found_len = 1;
            }
        }
    }
    if let Some(start) = free_start {
        if found_len >= size_pages {
            // starting point found, and enough pages
            assert!(
                found_len == size_pages,
                "Found pages should be exactly equal to size_pages at this point"
            );
            for i in ifram_allocs[start..start + found_len].iter_mut() {
                *i = Some(sender);
            }
            // offset relative to start of IFRAM bank
            Some(start * 4096)
        } else {
            // starting point found, but not enough pages
            None
        }
    } else {
        // no starting point found
        None
    }
}
fn main() {
    log_server::init_wait().unwrap();
    log::set_max_level(log::LevelFilter::Debug);

    let xns = xous_api_names::XousNames::new().unwrap();
    let sid = xns.register_name(cram_hal_service::SERVER_NAME_CRAM_HAL, None).expect("can't register server");

    let mut ifram_allocs = [Vec::new(), Vec::new()];
    // code is written assuming the IFRAM blocks have the same size. Since this is fixed in
    // hardware, it's a good assumption; but the assert is put here in case we port this to
    // a new system where for some reason they have different sizes.
    assert!(utralib::generated::HW_IFRAM0_MEM_LEN == utralib::generated::HW_IFRAM1_MEM_LEN);
    let pages = utralib::generated::HW_IFRAM0_MEM_LEN / 4096;
    for _ in 0..pages {
        ifram_allocs[0].push(None);
        ifram_allocs[1].push(None);
    }
    // Top page of IFRAM0 is occupied by the log server's Tx buffer. We can't know the
    // `Sender` of it, so fill it with a value for `Some` that can't map to any PID.
    ifram_allocs[0][31] = Some(Sender::from_usize(usize::MAX));

    let iox_page = xous::syscall::map_memory(
        xous::MemoryAddress::new(utralib::generated::HW_IOX_BASE),
        None,
        4096,
        xous::MemoryFlags::R | xous::MemoryFlags::W,
    )
    .expect("couldn't claim the IOX hardware page");
    let mut iox = iox::Iox::new(iox_page.as_mut_ptr() as *mut u32);

    let udma_global_csr = xous::syscall::map_memory(
        xous::MemoryAddress::new(utralib::generated::HW_UDMA_CTRL_BASE),
        None,
        4096,
        xous::MemoryFlags::R | xous::MemoryFlags::W,
    )
    .expect("couldn't map UDMA global control");
    let mut udma_global = GlobalConfig::new(udma_global_csr.as_mut_ptr() as *mut u32);

    let mut msg_opt = None;
    log::debug!("Starting main loop");
    loop {
        xous::reply_and_receive_next(sid, &mut msg_opt).unwrap();
        let msg = msg_opt.as_mut().unwrap();
        let opcode = num_traits::FromPrimitive::from_usize(msg.body.id()).unwrap_or(api::Opcode::InvalidCall);
        log::debug!("{:?}", opcode);
        match opcode {
            Opcode::MapIfram => {
                if let Some(scalar) = msg.body.scalar_message_mut() {
                    let requested_size = scalar.arg1; // requested size
                    let requested_bank = scalar.arg2; // Specifies bank 0, 1, or don't care (any number but 0 or 1)

                    let mut allocated_address = None;
                    for (bank, table) in ifram_allocs.iter_mut().enumerate() {
                        if bank == requested_bank || requested_bank > 1 {
                            match try_alloc(table, requested_size, msg.sender) {
                                Some(offset) => {
                                    let base = if bank == 0 {
                                        utralib::generated::HW_IFRAM0_MEM
                                    } else {
                                        utralib::generated::HW_IFRAM1_MEM
                                    };
                                    allocated_address = Some(base + offset);
                                    break;
                                }
                                None => {}
                            }
                        }
                    }
                    // responds with size in arg1 (0 means could not be allocated/OOM)
                    // and address of allocation in arg2
                    if let Some(addr) = allocated_address {
                        log::debug!(
                            "Allocated IFRAM at 0x{:x} to hold at least 0x{:x} bytes",
                            addr,
                            requested_size
                        );
                        log::debug!("Alloc[0]: {:x?}", ifram_allocs[0]);
                        log::debug!("Alloc[1]: {:x?}", ifram_allocs[1]);
                        scalar.arg1 = requested_size;
                        scalar.arg2 = addr;
                    } else {
                        log::debug!(
                            "Could not allocate IFRAM request of 0x{:x} bytes in bank {}",
                            requested_size,
                            requested_bank
                        );
                        scalar.arg1 = 0;
                        scalar.arg2 = 0;
                    }
                }
            }
            Opcode::UnmapIfram => {
                if let Some(scalar) = msg.body.scalar_message() {
                    let mapped_size = scalar.arg1;
                    let phys_addr = scalar.arg2;

                    let bank: usize;
                    let offset = if utralib::generated::HW_IFRAM0_MEM <= phys_addr
                        && phys_addr
                            < utralib::generated::HW_IFRAM0_MEM + utralib::generated::HW_IFRAM0_MEM_LEN
                    {
                        bank = 0;
                        phys_addr - utralib::generated::HW_IFRAM0_MEM
                    } else if utralib::generated::HW_IFRAM1_MEM <= phys_addr
                        && phys_addr
                            < utralib::generated::HW_IFRAM1_MEM + utralib::generated::HW_IFRAM1_MEM_LEN
                    {
                        bank = 1;
                        phys_addr - utralib::generated::HW_IFRAM1_MEM
                    } else {
                        log::error!("Mapped IFRAM address 0x{:x} is invalid", phys_addr);
                        panic!("Mapped IFRAM address is invalid");
                    };
                    let mut mapped_pages = mapped_size / 4096;
                    if mapped_size % 4096 != 0 {
                        mapped_pages += 1;
                    }
                    for record in ifram_allocs[bank][offset..offset + mapped_pages].iter_mut() {
                        *record = None;
                    }
                }
            }
            Opcode::ConfigureIox => {
                let buf =
                    unsafe { xous_ipc::Buffer::from_memory_message(msg.body.memory_message().unwrap()) };
                let config = buf.to_original::<IoxConfigMessage, _>().unwrap();
                if let Some(f) = config.function {
                    iox.set_alternate_function(config.port, config.pin, f);
                }
                if let Some(d) = config.direction {
                    iox.set_gpio_dir(config.port, config.pin, d);
                }
                if let Some(t) = config.schmitt_trigger {
                    iox.set_gpio_schmitt_trigger(config.port, config.pin, t);
                }
                if let Some(p) = config.pullup {
                    iox.set_gpio_pullup(config.port, config.pin, p);
                }
                if let Some(s) = config.slow_slew {
                    iox.set_slow_slew_rate(config.port, config.pin, s);
                }
                if let Some(s) = config.strength {
                    iox.set_drive_strength(config.port, config.pin, s);
                }
            }
            Opcode::SetGpioBank => {
                if let Some(scalar) = msg.body.scalar_message() {
                    let port: cramium_hal::iox::IoxPort =
                        num_traits::FromPrimitive::from_usize(scalar.arg1).unwrap();
                    let value = scalar.arg2 as u16;
                    let bitmask = scalar.arg3 as u16;
                    iox.set_gpio_bank(port, value, bitmask);
                }
            }
            Opcode::GetGpioBank => {
                if let Some(scalar) = msg.body.scalar_message_mut() {
                    let port: cramium_hal::iox::IoxPort =
                        num_traits::FromPrimitive::from_usize(scalar.arg1).unwrap();
                    scalar.arg1 = iox.get_gpio_bank(port) as usize;
                }
            }
            Opcode::ConfigureUdmaClock => {
                if let Some(scalar) = msg.body.scalar_message() {
                    let periph: PeriphId = num_traits::FromPrimitive::from_usize(scalar.arg1).unwrap();
                    let enable = if scalar.arg2 != 0 { true } else { false };
                    if enable {
                        udma_global.clock_on(periph);
                    } else {
                        udma_global.clock_off(periph);
                    }
                }
            }
            Opcode::ConfigureUdmaEvent => {
                if let Some(scalar) = msg.body.scalar_message() {
                    let periph: PeriphId = num_traits::FromPrimitive::from_usize(scalar.arg1).unwrap();
                    let event_offset = scalar.arg2 as u32;
                    let to_channel: EventChannel =
                        num_traits::FromPrimitive::from_usize(scalar.arg3).unwrap();
                    // note: no "air traffic control" is done to prevent mapping other
                    // events. Maybe this should be done? but for now, let's leave it
                    // as bare iron.
                    udma_global.map_event_with_offset(periph, event_offset, to_channel);
                }
            }
            Opcode::InvalidCall => {
                log::error!("Invalid opcode received: {:?}", msg);
            }
            Opcode::Quit => {
                log::info!("Received quit opcode, exiting.");
                break;
            }
        }
    }
    xns.unregister_server(sid).unwrap();
    xous::destroy_server(sid).unwrap();
    log::trace!("quitting");
    xous::terminate_process(0)
}
