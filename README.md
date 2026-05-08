# hosttui

A terminal UI to manage SSH hosts: browse, organize in groups, connect, and transfer files.

## Features

- **Browse hosts** in a grouped, scrollable list with a detail pane
- **Groups** — create, delete, and filter hosts by group
- **CRUD** — add, edit, and delete hosts via a form overlay with validation
- **Multi-session** — open multiple SSH sessions in tabs and switch between them without disconnecting
- **Connect** — press Enter to SSH into a host; the TUI suspends cleanly and restores on exit
- **File transfer** — dual-pane SFTP browser (local + remote) with background uploads/downloads and progress bars
- **SSH config generation** — auto-generates `~/.ssh/config.hosttui` on every change
- **Persistent config** — hosts are stored in `~/.config/hosttui/hosts.toml` with atomic writes

## Install

### From [crates.io](https://crates.io)

```
cargo install hosttui
```

### From source

```
cargo install --path .
```

### From GitHub releases

Download a prebuilt binary from the [releases page](https://github.com/yagoliz/hosttui/releases), extract, and place it in your `PATH`.

## Usage

```
ht (hosttui's short name)
```

### Key bindings

| Key | Action |
|---|---|
| `j` / `↓` | Move down |
| `k` / `↑` | Move up |
| `Tab` | Switch focus between Groups and Hosts panes |
| `Enter` | Connect to selected host via SSH |
| `t` | Test connection to host |
| `a` | Add a new host |
| `e` | Edit selected host |
| `d` | Delete selected host or group |
| `g` | Create a new group (when Groups pane is focused) |
| `f` | Open file transfer browser for selected host |
| `q` / `Esc` | Quit |

### Tab navigation

When you have active SSH sessions, a tab bar appears at the bottom. Use `Ctrl+T` as a prefix key followed by a command:

| Key | Action |
|---|---|
| `Ctrl+T h` / `Ctrl+T 0` | Switch to Hosts tab |
| `Ctrl+T 1-9` | Switch to session N |
| `Ctrl+T n` | Next tab |
| `Ctrl+T p` | Previous tab |
| `Ctrl+T x` | Close current session |
| `Ctrl+T ?` | Show tab help |
| `Ctrl+T f` | Open file transfer browser for the session's host |
| `Ctrl+T Ctrl+T` | Send literal `Ctrl+T` to session |

### File transfer navigation

Press `f` in the hosts view (or `Ctrl+T f` in a session) to open a dual-pane SFTP browser. The left pane shows the local filesystem; the right pane shows the remote.

| Key | Action |
|---|---|
| `j` / `↓` | Move cursor down |
| `k` / `↑` | Move cursor up |
| `Tab` | Switch focus between Local and Remote pane |
| `Enter` | Enter directory / initiate transfer if file |
| `Backspace` / `h` | Go to parent directory |
| `y` | Transfer focused file to the other pane |
| `m` | Create directory |
| `d` | Delete file or directory (with confirmation) |
| `r` | Refresh both pane listings |
| `.` | Toggle hidden files |
| `s` | Cycle sort field (Name → Size → Modified) |
| `Esc` | Close file transfer tab |

Transfers run in the background. A progress bar at the bottom of the view shows status while a transfer is active.

### Form navigation

| Key | Action |
|---|---|
| `Tab` | Next field |
| `Shift+Tab` | Previous field |
| `Enter` | Save |
| `Esc` | Cancel |

## SSH config integration

hosttui generates `~/.ssh/config.hosttui` every time you save a change. To use it, add this to your `~/.ssh/config`:

```
Match all
Include config.hosttui
```

## Config format

Hosts are stored in TOML at `~/.config/hosttui/hosts.toml`:

```toml
[[groups]]
name = "production"

[[hosts]]
alias = "web1"
hostname = "10.0.1.10"
user = "deploy"
port = 22
group = "production"
identity_file = "~/.ssh/prod_key"
extra = []
```

## License

MIT
