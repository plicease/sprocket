// SPDX-License-Identifier: MIT
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use clap::{Args, Parser, Subcommand, ValueEnum};

use sprocket::files::Root;
use sprocket::trace::Trace;
use sprocket::xmodem::{self, Event, RetryReason};
use sprocket::{serve, transport, zmodem};

#[derive(Parser)]
#[command(version, about = "Serial Protocol Rocket")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Send a file to a receiver using a standard serial transfer protocol
    Send(SendArgs),
    /// Serve a read-only file menu to a terminal program on the serial line
    Serve(ServeArgs),
}

#[derive(Args)]
struct SendArgs {
    /// Transfer protocol
    #[arg(long, value_enum, default_value_t = Protocol::Xmodem)]
    protocol: Protocol,

    /// Serial device (e.g. /dev/ttyS0), or tcp://HOST:PORT for an emulator link
    #[arg(long)]
    port: String,

    /// Baud rate (serial devices only); the line is always 8N1, no flow control
    #[arg(long, default_value_t = 9600)]
    baud: u32,

    /// Seconds to wait for the receiver to start the transfer
    #[arg(long, default_value_t = 60)]
    wait: u64,

    /// File to send
    file: PathBuf,

    /// Append a timestamped byte-level trace of the link to FILE
    #[arg(long, value_name = "FILE")]
    trace: Option<PathBuf>,
}

#[derive(Args)]
struct ServeArgs {
    /// Serial device (e.g. /dev/ttyS0), or tcp://HOST:PORT for an emulator link
    #[arg(long)]
    port: String,

    /// Baud rate (serial devices only); the line is always 8N1, no flow control
    #[arg(long, default_value_t = 9600)]
    baud: u32,

    /// Directory to serve; nothing outside it is reachable
    #[arg(long)]
    root: PathBuf,

    /// Seconds without input before a session returns to the banner
    #[arg(long, default_value_t = 600)]
    idle: u64,

    /// Seconds a transfer waits for the receiver to start
    #[arg(long, default_value_t = 60)]
    wait: u64,

    /// Append a timestamped byte-level trace of the link to FILE
    #[arg(long, value_name = "FILE")]
    trace: Option<PathBuf>,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Protocol {
    Zmodem,
    Xmodem,
    #[value(name = "xmodem-1k")]
    Xmodem1k,
}

fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::Send(args) => send(args),
        Command::Serve(args) => serve(args),
    }
}

fn open_trace(path: Option<&PathBuf>, command: &str) -> anyhow::Result<Option<Trace>> {
    let Some(path) = path else {
        return Ok(None);
    };
    let trace =
        Trace::create(path).with_context(|| format!("cannot open trace {}", path.display()))?;
    trace.note(&format!("sprocket {} {command}", env!("CARGO_PKG_VERSION")));
    Ok(Some(trace))
}

/// Open the link, recording its traffic when tracing.
fn open_link(
    port: &str,
    baud: u32,
    trace: Option<&Trace>,
) -> io::Result<Box<dyn transport::Transport>> {
    let link = transport::open(port, baud)?;
    Ok(match trace {
        Some(trace) => {
            trace.note(&format!("opened {port} at {baud} 8N1"));
            trace.wrap(link)
        }
        None => link,
    })
}

fn serve(args: ServeArgs) -> anyhow::Result<()> {
    let root =
        Root::new(&args.root).with_context(|| format!("cannot serve {}", args.root.display()))?;
    let trace = open_trace(args.trace.as_ref(), "serve")?;
    // Host log messages go to stderr and, when tracing, into the trace.
    let mut log = |msg: &str| {
        eprintln!("{msg}");
        if let Some(trace) = &trace {
            trace.note(msg);
        }
    };
    let options = serve::Options {
        idle_timeout: Duration::from_secs(args.idle),
        start_timeout: Duration::from_secs(args.wait),
    };
    log(&format!(
        "serving {} on {} ({} 8N1, no flow control); press Ctrl-C to stop",
        root.path().display(),
        args.port,
        args.baud
    ));
    // Keep serving across emulator restarts and dropped TCP links.
    loop {
        match open_link(&args.port, args.baud, trace.as_ref()) {
            Ok(mut link) => {
                log(&format!("connected to {}", args.port));
                if let Err(e) = serve::run(link.as_mut(), &root, &options, &mut log) {
                    log(&format!("connection lost: {e}"));
                }
            }
            Err(e) => log(&format!("cannot open {}: {e}", args.port)),
        }
        std::thread::sleep(Duration::from_secs(5));
    }
}

fn send(args: SendArgs) -> anyhow::Result<()> {
    let data =
        fs::read(&args.file).with_context(|| format!("cannot read {}", args.file.display()))?;
    let trace = open_trace(
        args.trace.as_ref(),
        &format!("send {}", args.file.display()),
    )?;
    let mut link = open_link(&args.port, args.baud, trace.as_ref())
        .with_context(|| format!("cannot open {}", args.port))?;

    let result = if args.protocol == Protocol::Zmodem {
        send_zmodem(&args, link.as_mut(), &data)
    } else {
        send_xmodem(&args, link.as_mut(), &data)
    };
    if let Some(trace) = &trace {
        match &result {
            Ok(()) => trace.note("transfer complete"),
            Err(e) => trace.note(&format!("{e:#}")),
        }
    }
    result
}

fn send_xmodem(
    args: &SendArgs,
    link: &mut dyn transport::Transport,
    data: &[u8],
) -> anyhow::Result<()> {
    let variant = match args.protocol {
        Protocol::Xmodem1k => xmodem::Variant::Xmodem1k,
        _ => xmodem::Variant::Xmodem,
    };
    let options = xmodem::Options {
        start_timeout: Duration::from_secs(args.wait),
        ..xmodem::Options::new(variant)
    };

    eprintln!(
        "sending {} ({} bytes) on {}; waiting up to {}s for the receiver to start",
        args.file.display(),
        data.len(),
        args.port,
        args.wait,
    );

    let tty = io::stderr().is_terminal();
    let mut mid_line = false;
    let summary = xmodem::send(link, data, &options, |event| match *event {
        Event::Started {
            checksum,
            block_size,
        } => {
            eprintln!("receiver started: {checksum}, {block_size}-byte blocks");
            if variant == xmodem::Variant::Xmodem1k && block_size != 1024 {
                eprintln!("note: receiver did not request CRC; using 128-byte blocks");
            }
        }
        Event::BlockAcked { sent, total, .. } if tty => {
            let percent = (sent * 100).checked_div(total).unwrap_or(100);
            eprint!("\r{sent}/{total} bytes ({percent}%)");
            let _ = io::stderr().flush();
            mid_line = true;
        }
        Event::BlockAcked { .. } => {}
        Event::Retry { block, reason } => {
            if mid_line {
                eprintln!();
                mid_line = false;
            }
            let what = if block == 0 {
                "EOT".to_string()
            } else {
                format!("block {block}")
            };
            let why = match reason {
                RetryReason::Nak => "NAK",
                RetryReason::Timeout => "timeout",
                RetryReason::StartRequest => "receiver repeated its start request",
            };
            eprintln!("retrying {what} ({why})");
        }
    });
    if mid_line {
        eprintln!();
    }
    let summary = summary.context("XMODEM transfer failed")?;

    eprintln!(
        "transfer complete: {} bytes in {} blocks, {}",
        summary.bytes, summary.blocks, summary.checksum
    );
    if summary.padding > 0 {
        eprintln!(
            "note: the final block was padded with {} SUB (0x1A) bytes; the received file \
             will be {} bytes. The original is exactly {} bytes; truncate before use if needed.",
            summary.padding,
            summary.bytes + summary.padding,
            summary.bytes,
        );
    }
    eprintln!(
        "note: XMODEM acknowledgements do not prove the saved file is byte-identical; verify on the receiver."
    );
    Ok(())
}

fn send_zmodem(
    args: &SendArgs,
    link: &mut dyn transport::Transport,
    data: &[u8],
) -> anyhow::Result<()> {
    let name = args
        .file
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let options = zmodem::Options {
        start_timeout: Duration::from_secs(args.wait),
        ..zmodem::Options::default()
    };
    eprintln!(
        "sending {} ({} bytes) by ZMODEM on {}; waiting up to {}s for the receiver",
        args.file.display(),
        data.len(),
        args.port,
        args.wait,
    );
    let tty = io::stderr().is_terminal();
    let modified = fs::metadata(&args.file).and_then(|m| m.modified()).ok();
    let summary = zmodem::send(
        link,
        &zmodem::wire_name(&name),
        data,
        modified,
        &options,
        |event| {
            let zmodem::Event::Progress { sent, total } = *event;
            if tty {
                let percent = (sent * 100).checked_div(total).unwrap_or(100);
                eprint!("\r{sent}/{total} bytes ({percent}%)");
                let _ = io::stderr().flush();
            }
        },
    );
    if tty {
        eprintln!();
    }
    if let Err(zmodem::Error::Skipped) = &summary {
        eprintln!("note: {}", zmodem::SKIP_HINT);
    }
    let summary = summary.context("ZMODEM transfer failed")?;
    eprintln!("transfer complete: {} bytes", summary.bytes);
    Ok(())
}
