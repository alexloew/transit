# transit

Transit is a [Transmit](https://panic.com/transmit/)-like TUI file transfer tool built with Rust and [Ratatui](https://github.com/ratatui/ratatui).

It gives you a dual-pane terminal interface for browsing local files and a remote SSH destination, marking files, and uploading them with `scp`. Remote browsing uses `ssh`, so Transit works with your existing `~/.ssh/config`, SSH keys, and agent. The local pane includes a file type column so long filenames do not hide extensions.

## Requirements

- Rust
- OpenSSH `ssh` and `scp`
- Key-based or agent-backed SSH authentication

Password prompts are not handled inside the TUI. Configure SSH keys or an agent for the best experience.

## Run

```sh
cargo run
```

On first startup, Transit runs an init flow that asks you to verify:

- the remote SSH host
- the remote media path
- the default local source directory

The default init values are `~/Downloads` locally, no preset remote host, and `~` as the remote path. Enter the SSH host and destination path for your own server during init.

Init saves those settings to:

```sh
~/.config/transit/config
```

Run init again at any time with:

```sh
cargo run -- init
```

You can also override the saved remote host and path:

```sh
cargo run -- user@host /remote/path
```

Example overrides:

```sh
cargo run -- user@example /remote/media
cargo run -- user@example "/remote/media/TV Shows"
```

## Keybindings

- `tab`: switch between local and remote panes
- `j`/`k` or arrow keys: move selection
- `enter`: open selected directory
- `h`, left arrow, or backspace: go to parent directory
- `space`: mark or unmark a local item for upload
- `u`: upload marked items, or the selected local item if nothing is marked
- `t`: toggle the transfer queue page
- `x`: cancel the selected queued upload from the transfer queue page
- `r`: refresh the active pane
- `o`: edit the remote host
- `g`: go to a remote path
- `q` or `esc`: quit

## Transfer Notes

Uploads are sent through a sequential queue in a background worker with `scp -r`, so files and directories are both supported. Only one `scp` process runs at a time, and Transit automatically continues with the next queued item after each upload finishes or fails.

Starting an upload switches to the transfer queue page automatically. The transfer page shows the active upload, queued items, elapsed time, byte progress when available, and final success or failure for each item. Press `t` to switch between the browser and transfer queue page.

On the transfer queue page, use `j`/`k` or the arrow keys to select an item and `x` to cancel a queued upload before it starts. Active uploads are not interrupted.

## Configuration Notes

Remote hosts are sanitized before Transit starts `ssh` or `scp`: whitespace, shell metacharacters, paths, and leading `-` values are rejected. Pasted `ssh://host/path` values are normalized to just the host.

The remote path is trimmed and must be non-empty. The source directory is expanded from `~` and must exist before init can save it.