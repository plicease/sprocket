# Configuration files and running as a service

**Status:** Implemented (2026-11-21) in the modern `sprocket` command. `SPROCKET.EXE` on DOS does not read configuration files.

## `--config`

Every `sprocket` command (`send`, `serve`, `client`, `hello` and `image`) takes `--config FILE`, which reads options from a TOML file:

```sh
sprocket serve --config /etc/sprocket.toml
sprocket serve --config /etc/sprocket.toml --baud 38400
sprocket client --config ./sprocket.toml
sprocket send --config ./sprocket.toml FILE
sprocket hello --config ./sprocket.toml
sprocket image --config ./sprocket.toml --list
```

`--config` may come before or after the command (`sprocket --config FILE serve` works too). **No file is read without it.** `sprocket` never looks in `/etc`, `~/.config`, the working directory or anywhere else on its own, and without `--config` every command behaves exactly as before.

## The file

The file has a section for each command, named after it: `[send]`, `[serve]`, `[client]`, `[hello]` and `[image]`. A command reads only its own section, so one file can hold settings for all of them. Keys are the long option names (`max_units` may also be written `max-units`), and values are as on the command line:

| Kind | Options | TOML |
| --- | --- | --- |
| Text | `port` | `port = "/dev/ttyS0"` |
| Numbers | `baud`, `wait`, `idle`, `pings`, `retries`, `max_units`, `recover`, `device` | `baud = 38400` |
| Paths | `root`, `trace`, `output`, `file` | `root = "/srv/sprocket"` |
| Choices | `mode`, `editing`, `protocol` | `mode = "terminal"`, `editing = "off"`, `protocol = "xmodem-1k"` |
| Flags | `writable`, `list`, `yes` | `writable = true` |
| Lists | `disk` | `disk = ["a.img", "b.img"]` |
| Geometry | `geometry` | `geometry = "80/2/18"` |
| Floppy format | `format` | `format = "1.44m"` or `format = 1440` |

[`contrib/sprocket.toml`](../contrib/sprocket.toml) shows every key of every section, with the defaults.

Relative paths are taken relative to the working directory, as on the command line (for a systemd service that is `/` unless `WorkingDirectory=` says otherwise), so use absolute paths in a shared file.

### Precedence

For each option, the first of these that gives it wins:

1. the command line;
2. the environment (`SPROCKET_BAUD`, for `baud`);
3. the command's section of the `--config` file;
4. the built-in default.

An option given on the command line wins even when it is given at its default value: `--editing on` overrides `editing = "off"`. A list given on the command line replaces the file's list: `--disk c.img` serves only `c.img`, not the file's disks as well. A flag can only be turned on from the command line, so a file's `writable = true` or `yes = true` cannot be turned off by an option: change the file, or use another one.

### Required options and arguments

A value the command needs can come from either place: `serve` needs `root` (`--root`), `send` needs `file` (the `FILE` argument), and `image` needs `device` and `output` unless it is listing. If both leave it out, the command stops with an error naming both places, and exits with status 2, as for any command-line error.

### Options that cannot be combined

The checks that apply on the command line apply to the file as well, and to options split between the two:

- `serve`: `writable` and `disk` need native mode, wherever `mode`, `writable` and `disk` came from.
- `image`: `geometry` and `format` cannot both be in the file. Either one on the command line replaces both of the file's, so `--format 720` overrides `geometry = "80/2/18"`.
- `image`: `list = true` cannot be in the file alongside `device` or `output`. `--list` on the command line ignores the file's `device` and `output`, so a file set up for imaging can still list disks; `--device` or `--output` on the command line ignores the file's `list`.

### Errors

A file that cannot be read, is not valid TOML, has a section or key that does not exist, or has a value of the wrong type or outside the allowed range is an error, reported with its line and column, and the command exits with status 1. Misspellings are never ignored: `[serv]` or `roots = ...` is an error, not a setting that silently does nothing. Every section is checked, whichever command runs.

## Running `sprocket serve` as a systemd service

[`contrib/sprocket.service`](../contrib/sprocket.service) is an example unit that runs `sprocket serve --config /etc/sprocket.toml`. Nothing installs or enables it for you; `/etc/sprocket.toml` is the path this example uses, not a default. Adjust the paths to suit, and change `ExecStart` to `/usr/bin/sprocket` if `sprocket` came from the `.deb`.

1. Create a user for the service that may open serial ports. Serial devices usually belong to the `dialout` group (Debian, Ubuntu) or `uucp` (Arch and some others); `ls -l /dev/ttyS0` shows which.

   ```sh
   sudo useradd --system --no-create-home --shell /usr/sbin/nologin sprocket
   ```

   The unit gives this user the `dialout` group with `SupplementaryGroups=`; change it to your system's group.

2. Install the configuration, readable by the service. It holds no secrets, so it can belong to root and be readable by all:

   ```sh
   sudo install -m 644 contrib/sprocket.toml /etc/sprocket.toml
   sudoedit /etc/sprocket.toml
   ```

   Set `port`, `baud` and `root` in `[serve]`. The served directory must be readable by the `sprocket` user, and writable by it if `writable = true`.

3. Install the unit and start it:

   ```sh
   sudo install -m 644 contrib/sprocket.service /etc/systemd/system/sprocket.service
   sudo systemctl daemon-reload
   sudo systemctl enable --now sprocket
   ```

   The unit makes the file system read-only to the service (`ProtectSystem=strict`, `ProtectHome=read-only`). For a writable server, or a `trace` file, list those directories in `ReadWritePaths=`. Do not add `PrivateDevices=`, which would hide the serial port.

4. Watch it: `sprocket serve` logs each connection and error to standard error, which goes to the journal.

   ```sh
   journalctl -u sprocket -f
   ```

### Changing the settings

`sprocket` reads its configuration once, when it starts. To apply a change to `/etc/sprocket.toml`, restart the service:

```sh
sudoedit /etc/sprocket.toml
sudo systemctl restart sprocket   # a new process rereads the configuration
```

Restarting ends any session in progress. `systemctl daemon-reload` is only needed when the **unit file** changes, not the TOML file:

```sh
sudo systemctl edit --full sprocket   # or edit /etc/systemd/system/sprocket.service
sudo systemctl daemon-reload
sudo systemctl restart sprocket
```

If the configuration has an error, `sprocket` exits at once with the error in the journal, and systemd retries it every five seconds (`Restart=on-failure`) until the file is fixed. To check a file without disturbing the running service, run `sprocket hello --config /etc/sprocket.toml --port tcp://127.0.0.1:1`: it checks every section of the file and reports any error, and with a good file it just fails to connect.
