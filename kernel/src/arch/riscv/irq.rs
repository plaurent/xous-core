// SPDX-FileCopyrightText: 2020 Sean Cross <sean@xobs.io>
// SPDX-License-Identifier: Apache-2.0

use crate::arch::current_pid;
use crate::arch::exception::RiscvException;
use crate::arch::mem::MemoryMapping;
use crate::arch::process::{Process as ArchProcess, RETURN_FROM_EXCEPTION_HANDLER};
use crate::arch::process::{Thread, EXIT_THREAD, RETURN_FROM_ISR};
use crate::services::SystemServices;
use riscv::register::{scause, sepc, sstatus, stval, vexriscv::sim, vexriscv::sip};
use xous_kernel::{SysCall, PID, TID};

extern "Rust" {
    fn _xous_syscall_return_result(result: &xous_kernel::Result, context: &Thread) -> !;
}

// use RAM-based backing so this variable is automatically saved on suspend
static mut SIM_BACKING: usize = 0;
/// Disable external interrupts
pub fn disable_all_irqs() {
    unsafe { SIM_BACKING = sim::read() };
    sim::write(0x0);
}

/// Enable external interrupts
#[export_name = "_enable_all_irqs"]
pub extern "C" fn enable_all_irqs() {
    sim::write(unsafe { SIM_BACKING });
}

pub fn enable_irq(irq_no: usize) {
    // Note that the vexriscv "IRQ Mask" register is inverse-logic --
    // that is, setting a bit in the "mask" register unmasks (i.e. enables) it.
    sim::write(sim::read() | (1 << irq_no));
}

pub fn disable_irq(irq_no: usize) -> Result<(), xous_kernel::Error> {
    sim::write(sim::read() & !(1 << irq_no));
    Ok(())
}

static mut PREVIOUS_PAIR: Option<(PID, TID)> = None;

pub unsafe fn set_isr_return_pair(pid: PID, tid: TID) {
    PREVIOUS_PAIR = Some((pid, tid));
}

#[cfg(feature = "gdb-stub")]
pub unsafe fn take_isr_return_pair() -> Option<(PID, TID)> {
    PREVIOUS_PAIR.take()
}

/// Convert a RISC-V `Exception` into a Xous exception argument list.
fn generate_exception_args(ex: &RiscvException) -> Option<[usize; 3]> {
    match *ex {
        RiscvException::InstructionAddressMisaligned(epc, addr) => Some([
            xous_kernel::ExceptionType::InstructionAddressMisaligned as usize,
            epc,
            addr,
        ]),
        RiscvException::InstructionAccessFault(epc, addr) => Some([
            xous_kernel::ExceptionType::InstructionAccessFault as usize,
            epc,
            addr,
        ]),
        RiscvException::IllegalInstruction(epc, instruction) => Some([
            xous_kernel::ExceptionType::IllegalInstruction as usize,
            epc,
            instruction,
        ]),
        RiscvException::LoadAddressMisaligned(epc, addr) => Some([
            xous_kernel::ExceptionType::LoadAddressMisaligned as usize,
            epc,
            addr,
        ]),
        RiscvException::LoadAccessFault(epc, addr) => Some([
            xous_kernel::ExceptionType::LoadAccessFault as usize,
            epc,
            addr,
        ]),
        RiscvException::StoreAddressMisaligned(epc, addr) => Some([
            xous_kernel::ExceptionType::StoreAddressMisaligned as usize,
            epc,
            addr,
        ]),
        RiscvException::StoreAccessFault(epc, addr) => Some([
            xous_kernel::ExceptionType::StoreAccessFault as usize,
            epc,
            addr,
        ]),
        RiscvException::InstructionPageFault(epc, addr) => Some([
            xous_kernel::ExceptionType::InstructionPageFault as usize,
            epc,
            addr,
        ]),
        RiscvException::LoadPageFault(epc, addr) => Some([
            xous_kernel::ExceptionType::LoadPageFault as usize,
            epc,
            addr,
        ]),
        RiscvException::StorePageFault(epc, addr) => Some([
            xous_kernel::ExceptionType::StorePageFault as usize,
            epc,
            addr,
        ]),
        _ => None,
    }
}

/// Trap entry point rust (_start_trap_rust)
///
/// scause is read to determine the cause of the trap. The top bit indicates if
/// it's an interrupt or an exception. The result is converted to an element of
/// the Interrupt or Exception enum and passed to handle_interrupt or
/// handle_exception.
#[export_name = "_start_trap_rust"]
pub extern "C" fn trap_handler(
    a0: usize,
    a1: usize,
    a2: usize,
    a3: usize,
    a4: usize,
    a5: usize,
    a6: usize,
    a7: usize,
) -> ! {
    let sc = scause::read();

    // If we were previously in Supervisor mode and we've just tried to write to
    // invalid memory, then we likely blew out the stack.
    if cfg!(target_arch = "riscv32")
        && sstatus::read().spp() == sstatus::SPP::Supervisor
        && sc.bits() == 0xf
    {
        let pid = current_pid();
        let ex = RiscvException::from_regs(sc.bits(), sepc::read(), stval::read());
        print!("KERNEL({}): RISC-V fault: {} - ", pid, ex);
        panic!("Maybe ran out of kernel stack?");
    }

    let pid = current_pid();

    if (sc.bits() == 9) || (sc.bits() == 8) {
        // We got here because of an `ecall` instruction.  When we return, skip
        // past this instruction.  If this is a call such as `SwitchTo`, then we
        // will want to adjust the return value of the current process prior to
        // performing the switch in order to avoid constantly executing the same
        // instruction.
        let tid = ArchProcess::with_current_mut(|p| {
            p.current_thread_mut().sepc += 4;
            p.current_tid()
        });
        let call = SysCall::from_args(a0, a1, a2, a3, a4, a5, a6, a7).unwrap_or_else(|_| {
            ArchProcess::with_current_mut(|p| unsafe {
                _xous_syscall_return_result(
                    &xous_kernel::Result::Error(xous_kernel::Error::UnhandledSyscall),
                    p.current_thread(),
                )
            })
        });

        let response = crate::syscall::handle(pid, tid, unsafe { PREVIOUS_PAIR.is_some() }, call)
            .unwrap_or_else(xous_kernel::Result::Error);

        // println!("Syscall Result: {:?}", response);
        ArchProcess::with_current_mut(|p| {
            let thread = p.current_thread();
            // If we're resuming a process that was previously sleeping, restore the
            // thread context. Otherwise, keep the thread context the same and pass
            // the return values in 8 argument registers.
            if response == xous_kernel::Result::ResumeProcess {
                crate::arch::syscall::resume(current_pid().get() == 1, thread);
            } else {
                // println!("Returning to address {:08x}", thread.sepc);
                unsafe { _xous_syscall_return_result(&response, thread) };
            }
        });
    }

    let ex = RiscvException::from_regs(sc.bits(), sepc::read(), stval::read());
    // println!("ex: {:?}", ex);
    if sc.is_exception() {
        // See if it's a known exception, such as writing to a demand-paged area
        // or returning from a handler or thread. If so, handle the exception
        // and return right away.
        match ex {
            RiscvException::StorePageFault(_pc, addr)
            | RiscvException::LoadPageFault(_pc, addr) => {
                #[cfg(all(feature = "debug-print", feature = "print-panics"))]
                print!(
                    "KERNEL({}): RISC-V fault: {} @ {:08x}, addr {:08x} - ",
                    pid, ex, _pc, addr
                );
                crate::arch::mem::ensure_page_exists_inner(addr)
                    .map(|_new_page| {
                        #[cfg(all(feature = "debug-print", feature = "print-panics"))]
                        klog!("Handing page {:08x} to process", _new_page);
                        ArchProcess::with_current_mut(|process| {
                            crate::arch::syscall::resume(
                                current_pid().get() == 1,
                                process.current_thread(),
                            )
                        });
                    })
                    .ok(); // If this fails, fall through.
            }

            RiscvException::InstructionPageFault(RETURN_FROM_EXCEPTION_HANDLER, _offset) => {
                // This address indicates the exception handler
                SystemServices::with_mut(|ss| {
                    ss.finish_exception_handler_and_resume(pid)
                        .expect("unable to finish exception handler")
                });

                // Resume the new thread within the same process.
                ArchProcess::with_current_mut(|p| {
                    // Adjust the program counter by the amount returned by the exception handler
                    let pc_adjust = a0 as isize;
                    if pc_adjust < 0 {
                        p.current_thread_mut().sepc -= pc_adjust.abs() as usize;
                    } else {
                        p.current_thread_mut().sepc += pc_adjust.abs() as usize;
                    }

                    crate::arch::syscall::resume(pid.get() == 1, p.current_thread())
                });
            }

            RiscvException::InstructionPageFault(EXIT_THREAD, _offset) => {
                let tid = ArchProcess::with_current(|process| process.current_tid());

                // This address indicates a thread has exited. Destroy the thread.
                // This activates another thread within this process.
                if SystemServices::with_mut(|ss| ss.destroy_thread(pid, tid)).unwrap() {
                    crate::syscall::reset_switchto_caller();
                }

                // Resume the new thread within the same process.
                ArchProcess::with_current_mut(|p| {
                    crate::arch::syscall::resume(current_pid().get() == 1, p.current_thread())
                });
            }

            RiscvException::InstructionPageFault(RETURN_FROM_ISR, _offset) => {
                // If we hit this address, then an ISR has just returned.  Since
                // we're in an interrupt context, it is safe to access this
                // global variable.
                let (previous_pid, previous_context) = unsafe {
                    PREVIOUS_PAIR
                        .take()
                        .expect("got RETURN_FROM_ISR with no previous PID")
                };
                // println!(
                //     "ISR: Resuming previous pair of ({}, {})",
                //     previous_pid, previous_context
                // );
                // Switch to the previous process' address space.
                SystemServices::with_mut(|ss| {
                    ss.finish_callback_and_resume(previous_pid, previous_context)
                        .expect("unable to resume previous PID")
                });

                // Re-enable interrupts now that they're handled
                enable_all_irqs();

                ArchProcess::with_current_mut(|process| {
                    crate::arch::syscall::resume(current_pid().get() == 1, process.current_thread())
                });
            }

            _ => (),
        }

        // This exception is not due to something we're aware of. In this case,
        // determine if there is an exception handler in this particular program
        // and call that handler if so.
        if let Some(args) = generate_exception_args(&ex) {
            if let Some(handler) = SystemServices::with_mut(|ss| ss.begin_exception_handler(pid)) {
                klog!("Exception handler for process exists ({:x?})", handler);
                // If this is the sort of exception that may be able to be handled by
                // the userspace program, generate a list of arguments to pass to
                // the handler.
                // Invoke the handler in userspace and exit this exception handler.
                klog!(
                    "At start of exception, current thread was: {}",
                    SystemServices::with(|ss| ss.get_process(pid).unwrap().current_thread)
                );
                ArchProcess::with_current_mut(|process| {
                    crate::arch::syscall::invoke(
                        process.thread_mut(crate::arch::process::EXCEPTION_TID),
                        current_pid().get() == 1,
                        handler.pc,
                        handler.sp,
                        RETURN_FROM_EXCEPTION_HANDLER,
                        &args,
                    );
                    crate::arch::syscall::resume(
                        current_pid().get() == 1,
                        process.thread(crate::arch::process::EXCEPTION_TID),
                    )
                });
            }
        }

        let is_kernel_failure = sstatus::read().spp() == sstatus::SPP::Supervisor;
        // The exception was not handled. We should terminate the program here.
        // For now, let's halt the whole system instead so that it becomes
        // immediately obvious that we screwed up. On harware this will trigger
        // a watchdog reset.
        println!(
            "{}: CPU Exception on PID {}: {}",
            if is_kernel_failure {
                "!!! KERNEL FAILURE !!!"
            } else {
                "PROGRAM HALT"
            },
            pid,
            ex
        );
        ArchProcess::with_current(|process| {
            println!("Current thread {}:", process.current_tid());
            process.print_current_thread();
        });

        // If this is a failure in the kernel, go into an infinite loop
        MemoryMapping::current().print_map();
        if is_kernel_failure {
            #[allow(clippy::empty_loop)]
            loop {}
        }

        // If it's not a failure in the kernel, terminate or debug the current process.
        SystemServices::with_mut(|ss| {
            ss.terminate_process(pid)
                .expect("couldn't terminate current process");
            crate::syscall::reset_switchto_caller();
        });

        // Resume the parent process.
        ArchProcess::with_current_mut(|process| {
            crate::arch::syscall::resume(current_pid().get() == 1, process.current_thread())
        })
    } else {
        let irqs_pending = sip::read();
        // println!("irqs: {:x}", irqs_pending);

        // Safe to access globals since interrupts are disabled
        // when this function runs.
        unsafe {
            if PREVIOUS_PAIR.is_none() {
                let tid = crate::arch::process::current_tid();
                PREVIOUS_PAIR = Some((pid, tid));
            }
        }
        crate::irq::handle(irqs_pending).expect("Couldn't handle IRQ");
        ArchProcess::with_current_mut(|process| {
            crate::arch::syscall::resume(current_pid().get() == 1, process.current_thread())
        })
    }
}
