// SPDX-License-Identifier: MIT
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use clap::{Args, Parser, Subcommand, ValueEnum};

use sprocket::files::Root;
use sprocket::native::{self, session};
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
    /// Serve a directory to a SPROCKET client (native mode, the default) or
    /// to a terminal program (--mode terminal)
    Serve(ServeArgs),
    /// Connect to a SPROCKET server and browse it with ftp-style commands:
    /// get, put, ls, cd, and lls, lcd for this machine
    Client(ClientArgs),
    /// Connect to a SPROCKET server, show what it supports, and ping it
    Hello(HelloArgs),
}

#[derive(Args)]
struct ClientArgs {
    /// Serial device (e.g. /dev/ttyS0), or tcp://HOST:PORT for an emulator link
    #[arg(long)]
    port: String,

    /// Baud rate (serial devices only); the line is always 8N1, no flow control
    #[arg(long, default_value_t = 9600)]
    baud: u32,

    /// Seconds to keep calling the server before giving up
    #[arg(long, default_value_t = 60)]
    wait: u64,

    /// Initial line editing mode (history, Tab completion, Ctrl-D to quit)
    #[arg(long, value_enum, default_value_t = Switch::On)]
    editing: Switch,

    /// Append a timestamped byte-level trace of the link to FILE
    #[arg(long, value_name = "FILE")]
    trace: Option<PathBuf>,
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
    /// native: answer a SPROCKET client (`SPROCKET CLIENT`, `sprocket
    /// client`). terminal: an ftp-style prompt for a terminal program, with
    /// downloads by ZMODEM or XMODEM
    #[arg(long, value_enum, default_value_t = Mode::Native)]
    mode: Mode,

    /// Serial device (e.g. /dev/ttyS0), or tcp://HOST:PORT for an emulator link
    #[arg(long)]
    port: String,

    /// Baud rate (serial devices only); the line is always 8N1, no flow control
    #[arg(long, default_value_t = 9600)]
    baud: u32,

    /// Directory to serve; nothing outside it is reachable
    #[arg(long)]
    root: PathBuf,

    /// Accept files from the client (put), and let put -f replace them
    /// (native mode). Without it the directory is read-only
    #[arg(long)]
    writable: bool,

    /// Seconds without input before a session returns to the banner
    /// (terminal mode)
    #[arg(long, default_value_t = 600)]
    idle: u64,

    /// Seconds a transfer waits for the receiver to start (terminal mode)
    #[arg(long, default_value_t = 60)]
    wait: u64,

    /// Initial line editing mode for each session (history, Tab completion,
    /// Ctrl-D logout); off suits terminals that send odd keystrokes
    /// (terminal mode)
    #[arg(long, value_enum, default_value_t = Switch::On)]
    editing: Switch,

    /// Append a timestamped byte-level trace of the link to FILE
    #[arg(long, value_name = "FILE")]
    trace: Option<PathBuf>,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Mode {
    Native,
    Terminal,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Switch {
    On,
    Off,
}

#[derive(Args)]
struct HelloArgs {
    /// Serial device (e.g. /dev/ttyS0), or tcp://HOST:PORT for an emulator link
    #[arg(long)]
    port: String,

    /// Baud rate (serial devices only); the line is always 8N1, no flow control
    #[arg(long, default_value_t = 9600)]
    baud: u32,

    /// Seconds to keep calling the server before giving up
    #[arg(long, default_value_t = 60)]
    wait: u64,

    /// Number of full-size pings to send
    #[arg(long, default_value_t = 3)]
    pings: u32,

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
        Command::Client(args) => client(args),
        Command::Hello(args) => hello(args),
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
    if args.writable && args.mode == Mode::Terminal {
        anyhow::bail!("--writable needs --mode native; terminal mode is always read-only");
    }
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
        editing: args.editing == Switch::On,
    };
    let native = native::server::Options {
        writable: args.writable,
        ..native::server::Options::default()
    };
    let what = match (args.mode, args.writable) {
        (Mode::Terminal, _) => "to a terminal program",
        (Mode::Native, false) => "to SPROCKET clients, read-only",
        (Mode::Native, true) => "to SPROCKET clients, writable",
    };
    log(&format!(
        "serving {} {what} on {} ({} 8N1, no flow control); press Ctrl-C to stop",
        root.path().display(),
        args.port,
        args.baud
    ));
    // Keep serving across emulator restarts and dropped TCP links.
    loop {
        match open_link(&args.port, args.baud, trace.as_ref()) {
            Ok(mut link) => {
                log(&format!("connected to {}", args.port));
                let result = match args.mode {
                    Mode::Terminal => serve::run(link.as_mut(), &root, &options, &mut log),
                    Mode::Native => native::server::run(link.as_mut(), &root, &native, &mut log),
                };
                if let Err(e) = result {
                    log(&format!("connection lost: {e}"));
                }
            }
            Err(e) => log(&format!("cannot open {}: {e}", args.port)),
        }
        std::thread::sleep(Duration::from_secs(5));
    }
}

fn client(args: ClientArgs) -> anyhow::Result<()> {
    let trace = open_trace(args.trace.as_ref(), "client")?;
    let mut link = open_link(&args.port, args.baud, trace.as_ref())
        .with_context(|| format!("cannot open {}", args.port))?;
    let note = |msg: &str| {
        if let Some(trace) = &trace {
            trace.note(msg);
        }
    };
    let options = session::Options {
        baud: args.baud,
        wait: Duration::from_secs(args.wait),
        ..session::Options::default()
    };
    eprintln!(
        "calling the server on {}; waiting up to {}s",
        args.port, args.wait
    );
    let observer = |event: &native::link::Event| {
        if let native::link::Event::Retransmit { seq, attempt } = event
            && *seq != 0
        {
            note(&format!("retransmitting request {seq} (attempt {attempt})"));
        }
    };
    let session = session::Session::connect(link.as_mut(), &options, observer)
        .context("cannot start a session")?;
    let local = std::env::current_dir().context("cannot find the current directory")?;
    let options = sprocket::client::Options {
        editing: args.editing == Switch::On,
    };
    let mut terminal = sprocket::terminal::Terminal::new().context("cannot set up the terminal")?;
    let result = sprocket::client::run(&mut terminal, session, &local, &options);
    drop(terminal);
    if let Err(e) = &result {
        note(&format!("{e:#}"));
    }
    result
}

fn hello(args: HelloArgs) -> anyhow::Result<()> {
    let trace = open_trace(args.trace.as_ref(), "hello")?;
    let mut link = open_link(&args.port, args.baud, trace.as_ref())
        .with_context(|| format!("cannot open {}", args.port))?;
    let log = |msg: &str| {
        eprintln!("{msg}");
        if let Some(trace) = &trace {
            trace.note(msg);
        }
    };
    let options = session::Options {
        baud: args.baud,
        wait: Duration::from_secs(args.wait),
        ..session::Options::default()
    };
    eprintln!(
        "calling the server on {}; waiting up to {}s",
        args.port, args.wait
    );
    let result = hello_session(&args, link.as_mut(), &options, &log);
    if let Err(e) = &result
        && let Some(trace) = &trace
    {
        trace.note(&format!("{e:#}"));
    }
    result
}

fn hello_session(
    args: &HelloArgs,
    link: &mut dyn transport::Transport,
    options: &session::Options,
    log: &dyn Fn(&str),
) -> anyhow::Result<()> {
    let observer = |event: &native::link::Event| {
        use native::link::Event;
        match event {
            // Seq 0 is the HELLO, repeated until the server answers.
            Event::Retransmit { seq: 0, .. } | Event::Pending { .. } => {}
            Event::Retransmit { seq, attempt } => {
                log(&format!("retransmitting request {seq} (attempt {attempt})"))
            }
            Event::Ignored(frame) => log(&format!(
                "ignored frame type {:#04x} session {:#06x} seq {}",
                frame.kind, frame.session, frame.seq
            )),
            Event::Discarded(reason) => log(&format!("dropped a frame: {reason}")),
        }
    };
    let mut session =
        session::Session::connect(link, options, observer).context("cannot start a session")?;
    let client = session.client().clone();
    println!(
        "session {:04x}: server {} on {}, protocol {}.{}",
        session.session_id(),
        if client.version.is_empty() {
            "(no version)"
        } else {
            &client.version
        },
        client.platform,
        client.major,
        client.minor,
    );
    println!(
        "max payload: server {} bytes, negotiated {} bytes",
        client.max_payload,
        session.max_payload()
    );
    println!("capabilities: {}", client.capabilities);

    // Every byte value, so escaping is exercised in both directions.
    let data: Vec<u8> = (0..session.max_payload()).map(|i| i as u8).collect();
    for n in 1..=args.pings {
        let rtt = session
            .ping(&data)
            .with_context(|| format!("ping {n} failed"))?;
        println!(
            "ping {n}: {} bytes each way in {:.3}s",
            data.len(),
            rtt.as_secs_f64()
        );
    }
    session.close().context("CLOSE failed")?;
    println!("session closed");
    Ok(())
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
