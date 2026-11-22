// SPDX-License-Identifier: MIT
//! The file given by `--config`: TOML with a section for each subcommand.
//!
//! A value in a command's section stands in for an option that was not
//! given on the command line or through the environment (`SPROCKET_BAUD`),
//! and takes precedence over the built-in default. Unknown sections and
//! keys are errors, so a misspelling is never silently ignored.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Context;
use clap::parser::ValueSource;
use clap::{ArgMatches, ValueEnum};
use serde::Deserialize;
use serde::de::{self, Deserializer};

use crate::native::storage::Geometry;
use crate::{
    ClientArgs, Command, HelloArgs, ImageArgs, Mode, Protocol, SendArgs, ServeArgs, Switch,
};

/// The whole file. Every section is checked, whichever command runs.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct File {
    #[serde(default)]
    send: Send,
    #[serde(default)]
    serve: Serve,
    #[serde(default)]
    client: Client,
    #[serde(default)]
    hello: Hello,
    #[serde(default)]
    image: Image,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Send {
    #[serde(default, deserialize_with = "value_enum")]
    protocol: Option<Protocol>,
    port: Option<String>,
    baud: Option<u32>,
    wait: Option<u64>,
    file: Option<PathBuf>,
    trace: Option<PathBuf>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Serve {
    #[serde(default, deserialize_with = "value_enum")]
    mode: Option<Mode>,
    port: Option<String>,
    baud: Option<u32>,
    root: Option<PathBuf>,
    writable: Option<bool>,
    disk: Option<Vec<PathBuf>>,
    idle: Option<u64>,
    wait: Option<u64>,
    #[serde(default, deserialize_with = "value_enum")]
    editing: Option<Switch>,
    trace: Option<PathBuf>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Client {
    port: Option<String>,
    baud: Option<u32>,
    wait: Option<u64>,
    #[serde(default, deserialize_with = "value_enum")]
    editing: Option<Switch>,
    trace: Option<PathBuf>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Hello {
    port: Option<String>,
    baud: Option<u32>,
    wait: Option<u64>,
    pings: Option<u32>,
    trace: Option<PathBuf>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Image {
    port: Option<String>,
    baud: Option<u32>,
    wait: Option<u64>,
    list: Option<bool>,
    device: Option<u8>,
    output: Option<PathBuf>,
    retries: Option<u8>,
    #[serde(alias = "max-units")]
    max_units: Option<usize>,
    #[serde(default, deserialize_with = "geometry")]
    geometry: Option<Geometry>,
    #[serde(default, deserialize_with = "format")]
    format: Option<Geometry>,
    recover: Option<u32>,
    yes: Option<bool>,
    trace: Option<PathBuf>,
}

/// Read and check the file named by `--config`.
pub fn load(path: &Path) -> anyhow::Result<File> {
    let text = fs::read_to_string(path)
        .with_context(|| format!("cannot read config {}", path.display()))?;
    let file: File =
        toml::from_str(&text).with_context(|| format!("bad config {}", path.display()))?;
    file.image
        .check()
        .with_context(|| format!("bad config {}", path.display()))?;
    Ok(file)
}

impl Image {
    /// Options that the command line would refuse together.
    fn check(&self) -> anyhow::Result<()> {
        if self.geometry.is_some() && self.format.is_some() {
            anyhow::bail!("[image] sets both geometry and format; give only one");
        }
        if self.list == Some(true) && (self.device.is_some() || self.output.is_some()) {
            anyhow::bail!(
                "[image] sets list = true with device or output; give list or the others"
            );
        }
        Ok(())
    }
}

/// Fill in the command's options from its section of `file`. `matches`
/// are the subcommand's, to tell given options from defaults.
pub fn apply(command: &mut Command, m: &ArgMatches, file: File) {
    let layer = Layer(m);
    match command {
        Command::Send(args) => send(&layer, args, file.send),
        Command::Serve(args) => serve(&layer, args, file.serve),
        Command::Client(args) => client(&layer, args, file.client),
        Command::Hello(args) => hello(&layer, args, file.hello),
        Command::Image(args) => image(&layer, args, file.image),
    }
}

/// The command line and environment, which override the file.
struct Layer<'a>(&'a ArgMatches);

impl Layer<'_> {
    /// Whether the option was given, rather than left at its default.
    fn given(&self, id: &str) -> bool {
        matches!(
            self.0.value_source(id),
            Some(ValueSource::CommandLine | ValueSource::EnvVariable)
        )
    }

    /// Take `value` for an option with a default, unless it was given.
    fn set<T>(&self, id: &str, field: &mut T, value: Option<T>) {
        if let Some(value) = value
            && !self.given(id)
        {
            *field = value;
        }
    }

    /// Take `value` for an option without a default, unless it was given.
    fn set_opt<T>(&self, id: &str, field: &mut Option<T>, value: Option<T>) {
        if value.is_some() && !self.given(id) {
            *field = value;
        }
    }
}

fn send(l: &Layer, a: &mut SendArgs, c: Send) {
    l.set("protocol", &mut a.protocol, c.protocol);
    l.set("port", &mut a.port, c.port);
    l.set("baud", &mut a.baud, c.baud);
    l.set("wait", &mut a.wait, c.wait);
    l.set_opt("file", &mut a.file, c.file);
    l.set_opt("trace", &mut a.trace, c.trace);
}

fn serve(l: &Layer, a: &mut ServeArgs, c: Serve) {
    l.set("mode", &mut a.mode, c.mode);
    l.set("port", &mut a.port, c.port);
    l.set("baud", &mut a.baud, c.baud);
    l.set_opt("root", &mut a.root, c.root);
    l.set("writable", &mut a.writable, c.writable);
    // --disk on the command line replaces the file's list.
    l.set("disk", &mut a.disk, c.disk);
    l.set("idle", &mut a.idle, c.idle);
    l.set("wait", &mut a.wait, c.wait);
    l.set("editing", &mut a.editing, c.editing);
    l.set_opt("trace", &mut a.trace, c.trace);
}

fn client(l: &Layer, a: &mut ClientArgs, c: Client) {
    l.set("port", &mut a.port, c.port);
    l.set("baud", &mut a.baud, c.baud);
    l.set("wait", &mut a.wait, c.wait);
    l.set("editing", &mut a.editing, c.editing);
    l.set_opt("trace", &mut a.trace, c.trace);
}

fn hello(l: &Layer, a: &mut HelloArgs, c: Hello) {
    l.set("port", &mut a.port, c.port);
    l.set("baud", &mut a.baud, c.baud);
    l.set("wait", &mut a.wait, c.wait);
    l.set("pings", &mut a.pings, c.pings);
    l.set_opt("trace", &mut a.trace, c.trace);
}

fn image(l: &Layer, a: &mut ImageArgs, c: Image) {
    l.set("port", &mut a.port, c.port);
    l.set("baud", &mut a.baud, c.baud);
    l.set("wait", &mut a.wait, c.wait);
    l.set("retries", &mut a.retries, c.retries);
    l.set("max_units", &mut a.max_units, c.max_units);
    l.set("recover", &mut a.recover, c.recover);
    l.set("yes", &mut a.yes, c.yes);
    l.set_opt("trace", &mut a.trace, c.trace);

    // geometry and format both say which geometry to image by: either on
    // the command line replaces the file's.
    if !l.given("geometry") && !l.given("format") {
        a.geometry = c.geometry;
        a.format = c.format;
    }

    // --list on the command line ignores the file's device and output, and
    // --device or --output ignores the file's list.
    let listing = c.list == Some(true);
    if !l.given("list") {
        if !l.given("device") && !l.given("output") {
            a.list = listing;
        }
        l.set_opt("device", &mut a.device, c.device);
        l.set_opt("output", &mut a.output, c.output);
    }
}

/// A value-enum option by the name it takes on the command line.
fn value_enum<'de, D: Deserializer<'de>, T: ValueEnum>(d: D) -> Result<Option<T>, D::Error> {
    let text = String::deserialize(d)?;
    T::from_str(&text, false).map(Some).map_err(|_| {
        let names: Vec<String> = T::value_variants()
            .iter()
            .filter_map(|v| v.to_possible_value())
            .map(|v| format!("{:?}", v.get_name()))
            .collect();
        de::Error::custom(format!(
            "invalid value {text:?}; expected {}",
            names.join(", ")
        ))
    })
}

/// `"C/H/S"`, as for --geometry.
fn geometry<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Geometry>, D::Error> {
    let text = String::deserialize(d)?;
    crate::parse_geometry(&text)
        .map(Some)
        .map_err(de::Error::custom)
}

/// A floppy format as for --format, as a string or a number of KB.
fn format<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Geometry>, D::Error> {
    struct Format;
    impl de::Visitor<'_> for Format {
        type Value = Geometry;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a floppy format such as \"1.44m\" or 1440")
        }
        fn visit_str<E: de::Error>(self, text: &str) -> Result<Geometry, E> {
            crate::parse_format(text).map_err(E::custom)
        }
        fn visit_i64<E: de::Error>(self, n: i64) -> Result<Geometry, E> {
            self.visit_str(&n.to_string())
        }
    }
    d.deserialize_any(Format).map(Some)
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use clap::error::ErrorKind;

    use crate::{Cli, Command, Mode, Protocol, Switch, resolve};

    /// Resolve `args` with `toml` as the --config file. Baud is left out of
    /// these tests, as SPROCKET_BAUD may be set; tests/config.rs covers it.
    fn with(toml: &str, args: &[&str]) -> anyhow::Result<Cli> {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(toml.as_bytes()).unwrap();
        let path = file.path().to_str().unwrap().to_string();
        let mut argv = vec!["sprocket", args[0], "--config", &path];
        argv.extend(&args[1..]);
        resolve(argv)
    }

    fn clap_kind(e: &anyhow::Error) -> Option<ErrorKind> {
        e.downcast_ref::<clap::Error>().map(|e| e.kind())
    }

    #[test]
    fn no_config_keeps_defaults_and_requirements() {
        let Command::Serve(a) = resolve(["sprocket", "serve", "--root", "r"])
            .unwrap()
            .command
        else {
            panic!()
        };
        assert_eq!(
            (a.mode, a.editing, a.writable, a.idle, a.wait),
            (Mode::Native, Switch::On, false, 600, 60)
        );
        assert_eq!(a.port, crate::DEFAULT_PORT);
        for args in [
            &["sprocket", "serve"][..],
            &["sprocket", "send"],
            &["sprocket", "image"],
            &["sprocket", "image", "--device", "0"],
        ] {
            let e = resolve(args).err().unwrap();
            assert_eq!(
                clap_kind(&e),
                Some(ErrorKind::MissingRequiredArgument),
                "{args:?}"
            );
        }
    }

    #[test]
    fn empty_config_changes_nothing() {
        let Command::Hello(a) = with("", &["hello"]).unwrap().command else {
            panic!()
        };
        assert_eq!(
            (a.port.as_str(), a.wait, a.pings),
            (crate::DEFAULT_PORT, 60, 3)
        );
        assert!(a.trace.is_none());
    }

    #[test]
    fn serve_from_config() {
        let toml = r#"
            [serve]
            mode = "terminal"
            port = "tcp://localhost:5000"
            root = "/srv/files"
            idle = 30
            wait = 5
            editing = "off"
            trace = "/tmp/t"
            [client]
            port = "elsewhere"
        "#;
        let Command::Serve(a) = with(toml, &["serve"]).unwrap().command else {
            panic!()
        };
        assert_eq!(a.mode, Mode::Terminal);
        assert_eq!(a.port, "tcp://localhost:5000");
        assert_eq!(a.root.unwrap().to_str(), Some("/srv/files"));
        assert_eq!((a.idle, a.wait, a.editing), (30, 5, Switch::Off));
        assert_eq!(a.trace.unwrap().to_str(), Some("/tmp/t"));
    }

    #[test]
    fn command_line_overrides_config_even_at_the_default() {
        let toml = r#"
            [serve]
            mode = "terminal"
            root = "/srv/files"
            editing = "off"
            wait = 5
            port = "/dev/ttyUSB0"
        "#;
        let args = [
            "serve",
            "--mode",
            "native",
            "--editing",
            "on",
            "--wait",
            "60",
            "--root",
            "here",
        ];
        let Command::Serve(a) = with(toml, &args).unwrap().command else {
            panic!()
        };
        assert_eq!((a.mode, a.editing, a.wait), (Mode::Native, Switch::On, 60));
        assert_eq!(a.root.unwrap().to_str(), Some("here"));
        assert_eq!(a.port, "/dev/ttyUSB0");
    }

    #[test]
    fn booleans_and_lists() {
        let toml = r#"
            [serve]
            root = "r"
            writable = true
            disk = ["a.img", "b.img"]
        "#;
        let Command::Serve(a) = with(toml, &["serve"]).unwrap().command else {
            panic!()
        };
        assert!(a.writable);
        assert_eq!(a.disk.len(), 2);
        let Command::Serve(a) = with(toml, &["serve", "--disk", "c.img"]).unwrap().command else {
            panic!()
        };
        assert_eq!(a.disk, [std::path::PathBuf::from("c.img")]);
        let toml = "[serve]\nroot = \"r\"\nwritable = false\n";
        let Command::Serve(a) = with(toml, &["serve", "--writable"]).unwrap().command else {
            panic!()
        };
        assert!(a.writable);
    }

    #[test]
    fn config_cannot_bypass_serve_conflicts() {
        for toml in [
            "[serve]\nroot = \"r\"\nmode = \"terminal\"\nwritable = true\n",
            "[serve]\nroot = \"r\"\nmode = \"terminal\"\ndisk = [\"a.img\"]\n",
        ] {
            let Command::Serve(a) = with(toml, &["serve"]).unwrap().command else {
                panic!()
            };
            assert!(crate::check_serve(&a).is_err(), "{toml}");
        }
        let toml = "[serve]\nroot = \"r\"\nwritable = true\n";
        let Command::Serve(a) = with(toml, &["serve", "--mode", "terminal"])
            .unwrap()
            .command
        else {
            panic!()
        };
        assert!(crate::check_serve(&a).is_err());
    }

    #[test]
    fn send_from_config() {
        let toml = "[send]\nprotocol = \"xmodem-1k\"\nwait = 9\nfile = \"a.bin\"\n";
        let Command::Send(a) = with(toml, &["send"]).unwrap().command else {
            panic!()
        };
        assert_eq!((a.protocol, a.wait), (Protocol::Xmodem1k, 9));
        assert_eq!(a.file.unwrap().to_str(), Some("a.bin"));
        let Command::Send(a) = with(toml, &["send", "b.bin"]).unwrap().command else {
            panic!()
        };
        assert_eq!(a.file.unwrap().to_str(), Some("b.bin"));
        let e = with("[send]\nwait = 9\n", &["send"]).err().unwrap();
        assert_eq!(clap_kind(&e), Some(ErrorKind::MissingRequiredArgument));
    }

    #[test]
    fn client_and_hello_from_config() {
        let toml = "[client]\nwait = 7\nediting = \"off\"\n[hello]\npings = 10\nport = \"p\"\n";
        let Command::Client(a) = with(toml, &["client"]).unwrap().command else {
            panic!()
        };
        assert_eq!((a.wait, a.editing), (7, Switch::Off));
        let Command::Hello(a) = with(toml, &["hello", "--pings", "3"]).unwrap().command else {
            panic!()
        };
        assert_eq!((a.pings, a.port.as_str()), (3, "p"));
    }

    #[test]
    fn image_from_config() {
        let toml = r#"
            [image]
            device = 1
            output = "a.img"
            retries = 9
            max_units = 4
            recover = 2
            format = "1.44m"
            yes = true
        "#;
        let Command::Image(a) = with(toml, &["image"]).unwrap().command else {
            panic!()
        };
        assert_eq!(
            (a.device, a.retries, a.max_units, a.recover, a.yes),
            (Some(1), 9, 4, 2, true)
        );
        assert_eq!(a.format.map(|g| g.sectors), Some(18));
        assert!(!a.list && a.geometry.is_none());
        // geometry on the command line replaces the file's format.
        let Command::Image(a) = with(toml, &["image", "--geometry", "40/2/9"])
            .unwrap()
            .command
        else {
            panic!()
        };
        assert_eq!(
            (a.format.is_none(), a.geometry.map(|g| g.cylinders)),
            (true, Some(40))
        );
        let Command::Image(a) = with("[image]\nformat = 720\n", &["image", "--list"])
            .unwrap()
            .command
        else {
            panic!()
        };
        assert_eq!(a.format.map(|g| g.sectors), Some(9));
    }

    #[test]
    fn image_list_against_device_and_output() {
        let toml = "[image]\ndevice = 1\noutput = \"a.img\"\n";
        let Command::Image(a) = with(toml, &["image", "--list"]).unwrap().command else {
            panic!()
        };
        assert!(a.list && a.device.is_none() && a.output.is_none());
        let toml = "[image]\nlist = true\n";
        let Command::Image(a) = with(toml, &["image"]).unwrap().command else {
            panic!()
        };
        assert!(a.list);
        let args = ["image", "--device", "2", "--output", "b.img"];
        let Command::Image(a) = with(toml, &args).unwrap().command else {
            panic!()
        };
        assert!(!a.list && a.device == Some(2));
        let e = with(toml, &["image", "--device", "2"]).err().unwrap();
        assert_eq!(clap_kind(&e), Some(ErrorKind::MissingRequiredArgument));
        let e = with("[image]\nlist = true\ndevice = 1\n", &["image"])
            .err()
            .unwrap();
        assert!(format!("{e:#}").contains("list = true"), "{e:#}");
        // The command line's own conflicts still apply.
        assert!(with("", &["image", "--list", "--device", "1"]).is_err());
        let e = with("[image]\noutput = \"a.img\"\n", &["image"])
            .err()
            .unwrap();
        assert_eq!(clap_kind(&e), Some(ErrorKind::MissingRequiredArgument));
    }

    #[test]
    fn bad_files_are_rejected() {
        for (toml, expect) in [
            ("[serv]\nroot = \"r\"\n", "unknown field `serv`"),
            ("[serve]\nroots = \"r\"\n", "unknown field `roots`"),
            ("[serve]\nwait = \"soon\"\n", "line 2"),
            (
                "[serve]\nmode = \"telnet\"\n",
                "expected \"native\", \"terminal\"",
            ),
            ("[image]\nretries = 300\n", "line 2"),
            ("[image]\ngeometry = \"80/2\"\n", "cylinders/heads/sectors"),
            ("[image]\nformat = \"880\"\n", "unknown format"),
            (
                "[image]\nformat = 720\ngeometry = \"80/2/9\"\n",
                "both geometry and format",
            ),
            ("[hello\n", "TOML parse error"),
            ("[client]\nediting = true\n", "line 2"),
        ] {
            let e = with(toml, &["hello"])
                .err()
                .unwrap_or_else(|| panic!("{toml}"));
            assert!(format!("{e:#}").contains(expect), "{toml}: {e:#}");
        }
        let e = resolve(["sprocket", "hello", "--config", "/no/such/sprocket.toml"])
            .err()
            .unwrap();
        assert!(
            format!("{e:#}").contains("cannot read config /no/such/sprocket.toml"),
            "{e:#}"
        );
    }

    #[test]
    fn example_file_is_valid() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/contrib/sprocket.toml");
        super::load(std::path::Path::new(path)).unwrap();
        // Every key it shows commented out is a real one, with a good value.
        let all: String = std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|line| match line.strip_prefix("# ") {
                Some(key) if key.contains(" = ") => key,
                _ => line,
            })
            .map(|line| format!("{line}\n"))
            .collect();
        toml::from_str::<super::File>(&all).unwrap();
    }

    #[test]
    fn config_before_the_subcommand() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(b"[hello]\npings = 5\n").unwrap();
        let path = file.path().to_str().unwrap();
        let Command::Hello(a) = resolve(["sprocket", "--config", path, "hello"])
            .unwrap()
            .command
        else {
            panic!()
        };
        assert_eq!(a.pings, 5);
    }
}
