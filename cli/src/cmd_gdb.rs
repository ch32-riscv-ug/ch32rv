//! en: `gdb` (docs/cli.ja.md §4.6): a GDB Remote Serial Protocol server over TCP. Attaches to
//! a halted core (never modifying flash), then drives it via gdbstub's blocking event loop.
//! Connect from GDB with `target remote <listen>`.
//! ja: `gdb`。TCP 上の GDB Remote Serial Protocol server。halt した core に attach(flash を
//! 書き換えない)し gdbstub の blocking event loop で駆動する。`target remote <listen>` で接続。

use std::net::{TcpListener, TcpStream};
use std::process::ExitCode;
use std::time::Duration;

use ch32rv_contract::ErrorKind;
use ch32rv_debug::Ch32Target;
use ch32rv_wchlink::WchLink;
use gdbstub::conn::{Connection, ConnectionExt};
use gdbstub::stub::run_blocking::{BlockingEventLoop, Event, WaitForStopReasonError};
use gdbstub::stub::{DisconnectReason, GdbStub, SingleThreadStopReason};
use gdbstub::target::Target;

use crate::args::{Cli, GdbArgs};
use crate::cmd_probe::{fail, mode_str, select_entry};
use crate::parse;

pub fn gdb(cli: &Cli, args: &GdbArgs) -> ExitCode {
    const CMD: &str = "gdb";
    let entry = match select_entry(cli, CMD) {
        Ok(e) => e,
        Err(c) => return c,
    };
    if entry.mode != ch32rv_contract::ProbeMode::Riscv {
        return fail(
            cli,
            CMD,
            ErrorKind::CapabilityUnsupported,
            format!(
                "gdb needs a RISC-V-mode probe (this is {})",
                mode_str(entry.mode)
            ),
            None,
        );
    }
    let (speed, _warnings) = match parse::speed(&cli.speed) {
        Ok(v) => v,
        Err(m) => return fail(cli, CMD, ErrorKind::Usage, m, None),
    };

    // Open a raw link and attach so the gdb target OWNS the transport (no lifetime on the
    // target type, which BlockingEventLoop::Target requires). We detach on the way out.
    let mut link = match WchLink::open(&entry.dev) {
        Ok(l) => l,
        Err(e) => return fail(cli, CMD, ErrorKind::DeviceOpenFailed, e.to_string(), None),
    };
    link.set_timeout(Duration::from_millis(3000));
    let _ = link.detach_chip();
    let _ = link.set_speed_default(speed);
    let attach = match link.attach_chip() {
        Ok(a) => a,
        Err(e) => return fail(cli, CMD, ErrorKind::AttachFailed, e.to_string(), None),
    };
    let _ = link.set_speed(attach.family_byte, speed);

    // Listen for one GDB connection.
    let listener = match TcpListener::bind(&args.listen) {
        Ok(l) => l,
        Err(e) => {
            return fail(
                cli,
                CMD,
                ErrorKind::Usage,
                format!("bind {}: {e}", args.listen),
                None,
            );
        }
    };
    let local = listener
        .local_addr()
        .map(|a| a.to_string())
        .unwrap_or_else(|_| args.listen.clone());
    eprintln!("gdb: listening on {local} (connect with: target remote {local})");
    if args.no_flash {
        eprintln!("gdb: --no-flash: vFlash (load) is not offered");
    }

    let (stream, peer) = match listener.accept() {
        Ok(v) => v,
        Err(e) => return fail(cli, CMD, ErrorKind::Usage, format!("accept: {e}"), None),
    };
    eprintln!("gdb: client connected from {peer}");

    let mut target = match Ch32Target::new(link) {
        Ok(t) => t,
        Err(e) => {
            return fail(
                cli,
                CMD,
                ErrorKind::AttachFailed,
                format!("halt for gdb failed: {e}"),
                None,
            );
        }
    };

    let conn = GdbConn(stream);
    let gdb = GdbStub::new(conn);
    let outcome = gdb.run_blocking::<Ch32EventLoop<WchLink>>(&mut target);
    // Recover the link and detach cleanly (resumes the core).
    let mut link = target.into_inner();
    let _ = link.detach_chip();

    match outcome {
        Ok(reason) => {
            match reason {
                DisconnectReason::Disconnect => eprintln!("gdb: client disconnected"),
                DisconnectReason::TargetExited(c) => eprintln!("gdb: target exited ({c})"),
                DisconnectReason::TargetTerminated(s) => {
                    eprintln!("gdb: target terminated (signal {s:?})")
                }
                DisconnectReason::Kill => eprintln!("gdb: session killed"),
            }
            ExitCode::SUCCESS
        }
        Err(e) => fail(
            cli,
            CMD,
            ErrorKind::Internal,
            format!("gdb session error: {e}"),
            None,
        ),
    }
}

/// A gdbstub Connection over a TCP stream (byte-at-a-time, as gdbstub expects).
struct GdbConn(TcpStream);

impl Connection for GdbConn {
    type Error = std::io::Error;

    fn write(&mut self, byte: u8) -> Result<(), Self::Error> {
        std::io::Write::write_all(&mut self.0, &[byte])
    }

    fn write_all(&mut self, buf: &[u8]) -> Result<(), Self::Error> {
        std::io::Write::write_all(&mut self.0, buf)
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        std::io::Write::flush(&mut self.0)
    }
}

impl ConnectionExt for GdbConn {
    fn read(&mut self) -> Result<u8, Self::Error> {
        use std::io::Read;
        self.0.set_nonblocking(false)?;
        let mut b = [0u8; 1];
        self.0.read_exact(&mut b)?;
        Ok(b[0])
    }

    fn peek(&mut self) -> Result<Option<u8>, Self::Error> {
        self.0.set_nonblocking(true)?;
        let mut b = [0u8; 1];
        let r = match self.0.peek(&mut b) {
            Ok(0) => Ok(None),
            Ok(_) => Ok(Some(b[0])),
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
            Err(e) => Err(e),
        };
        let _ = self.0.set_nonblocking(false);
        r
    }
}

/// The blocking event loop: after `resume`, poll the core's halt state while watching for
/// incoming GDB data (Ctrl-C). When the core halts (an `ebreak` breakpoint, or a step
/// completing), report a SwBreak/DoneStep stop.
enum Ch32EventLoop<T: ch32rv_dmi::DtmAccess> {
    _Never(core::convert::Infallible, core::marker::PhantomData<T>),
}

impl<T: ch32rv_dmi::DtmAccess> BlockingEventLoop for Ch32EventLoop<T> {
    type Target = Ch32Target<T>;
    type Connection = GdbConn;
    type StopReason = SingleThreadStopReason<u32>;

    fn wait_for_stop_reason(
        target: &mut Self::Target,
        conn: &mut Self::Connection,
    ) -> Result<
        Event<Self::StopReason>,
        WaitForStopReasonError<
            <Self::Target as Target>::Error,
            <Self::Connection as Connection>::Error,
        >,
    > {
        loop {
            // Incoming data (e.g. Ctrl-C) takes priority.
            match conn.peek() {
                Ok(Some(_)) => {
                    let b =
                        ConnectionExt::read(conn).map_err(WaitForStopReasonError::Connection)?;
                    return Ok(Event::IncomingData(b));
                }
                Ok(None) => {}
                Err(e) => return Err(WaitForStopReasonError::Connection(e)),
            }
            // Poll the core.
            match target.is_halted() {
                Ok(true) => return Ok(Event::TargetStopped(SingleThreadStopReason::SwBreak(()))),
                Ok(false) => std::thread::sleep(Duration::from_millis(5)),
                Err(e) => return Err(WaitForStopReasonError::Target(e)),
            }
        }
    }

    fn on_interrupt(
        target: &mut Self::Target,
    ) -> Result<Option<Self::StopReason>, <Self::Target as Target>::Error> {
        // Halt the running core in response to Ctrl-C, then report SIGINT.
        target.halt()?;
        Ok(Some(SingleThreadStopReason::Signal(
            gdbstub::common::Signal::SIGINT,
        )))
    }
}
