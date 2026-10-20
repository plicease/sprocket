// SPDX-License-Identifier: MIT
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use clap::{Args, Parser, Subcommand, ValueEnum};

use sprocket::transport;
use sprocket::xmodem::{self, Event, RetryReason};

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
}

#[derive(Clone, Copy, ValueEnum)]
enum Protocol {
    Xmodem,
    #[value(name = "xmodem-1k")]
    Xmodem1k,
}

fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::Send(args) => send(args),
    }
}

fn send(args: SendArgs) -> anyhow::Result<()> {
    let data =
        fs::read(&args.file).with_context(|| format!("cannot read {}", args.file.display()))?;
    let mut link = transport::open(&args.port, args.baud)
        .with_context(|| format!("cannot open {}", args.port))?;

    let variant = match args.protocol {
        Protocol::Xmodem => xmodem::Variant::Xmodem,
        Protocol::Xmodem1k => xmodem::Variant::Xmodem1k,
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
    let summary = xmodem::send(link.as_mut(), &data, &options, |event| match *event {
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
