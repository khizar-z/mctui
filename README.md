
# mctui

**A live first-person Minecraft Java renderer for the terminal.**

<img width="800" height="519" alt="2026-08-2602-01-08-ezgif com-speed" src="https://github.com/user-attachments/assets/330568b6-eb5b-4371-8db9-a626832f2755" />

`mctui` connects to a local Minecraft server as a client, consumes the chunks
and lighting data it receives, and renders the world as a 24-bit ANSI view in
real time. It is written in Rust and uses a custom voxel raycaster rather than
capturing or proxying the official game client.

The project is a systems-focused exploration of real-time rendering on a text
terminal: networking state arrives asynchronously, blocks are raycast through
a streamed voxel world, and each terminal cell represents two independently
coloured pixels.

## Highlights

- Live Minecraft Java world data via [Azalea](https://github.com/azalea-rs/azalea)
- Custom Amanatides & Woo DDA voxel raycaster
- 24-bit ANSI rendering using foreground/background half-block characters
- Packet-backed block light, sky light, and server time-of-day
- Water, glass, ice, and lava transparency with camera-underwater overlays
- Interactive movement, sprinting, jumping, crouching, and look controls
- 15×15 navigation minimap that projects nearby terrain below the player
- Conservative unloaded-chunk handling: unknown terrain is never rendered as sky

```text
Minecraft packets → streamed chunk state + light cache → DDA raycaster → ANSI terminal frame
```

## Quick start

### Prerequisites

- Rust nightly, selected automatically by the included
  [`rust-toolchain.toml`](rust-toolchain.toml)
- A local Minecraft Java server compatible with Minecraft `26.2`
- A true-colour ANSI terminal (Kitty, iTerm2, WezTerm, Ghostty, macOS Terminal,
  and most modern Linux terminals work)

Start the server separately, then run mctui from this repository. The default
connection is an offline-mode local server at `127.0.0.1:25565`.

```sh
cargo run --release -- --mode render
```

Use a distinct username when another local player is already connected:

```sh
cargo run --release -- --username terminal_view --mode render
```

The client rejects non-loopback server addresses unless
`--allow-public-server` is supplied explicitly.

## Controls

| Input | Action |
| --- | --- |
| `W` / `A` / `S` / `D` | Move forward / left / backward / right |
| `Shift` + `W` | Sprint forward |
| Arrow keys | Look in 6° increments |
| `Space` | Jump or swim upward |
| `C` | Toggle crouch |
| `X` | Stop moving |
| `Q` or `Esc` | Quit |

In terminals that support Kitty keyboard enhancement, releasing a movement key
also stops movement. `X` is always available as a terminal-independent stop.

## Recommended command

This setting combines a larger viewport with terrain streamed and raycast
farther from the player:

```sh
cargo run --release -- --mode render \
  --width 120 --height 36 --fps 10 \
  --view-distance 12 --distance 80
```

Increase `--width` and `--height` for more detail; lower `--fps` or
`--distance` if rendering becomes too expensive. The navigation minimap uses
19 additional terminal columns, so a 120-column viewport needs a terminal
roughly 140 columns wide.

## CLI reference

| Flag | Default | Description |
| --- | --- | --- |
| `--server <HOST:PORT>` | `127.0.0.1:25565` | Minecraft server address |
| `--username <NAME>` | `mctui` | In-game username |
| `--mode <MODE>` | `render` | `monitor`, `minimap`, `ray`, or interactive `render` |
| `--width <COLS>` | `80` | Rendered viewport width in terminal columns |
| `--height <ROWS>` | `24` | Rendered viewport height in terminal rows |
| `--fps <FPS>` | `12` | Target interactive frame rate |
| `--distance <BLOCKS>` | `48` | Maximum DDA ray distance |
| `--fov <DEGREES>` | `75` | Horizontal field of view |
| `--view-distance <CHUNKS>` | `8` | Requested server view distance |
| `--entities` | off | Enable experimental nearby-entity markers |
| `--online` | off | Use online authentication instead of offline mode |
| `--allow-public-server` | off | Permit a non-loopback server address |

Run `cargo run --release -- --help` for the authoritative list of options.

### Render modes

| Mode | Purpose |
| --- | --- |
| `monitor` | Connection and player-position diagnostics |
| `minimap` | Top-down view of the loaded terrain around the player |
| `ray` | Inspect the first block encountered by a camera ray |
| `render` | Interactive first-person terminal renderer |

## How it works

### World and lighting

Azalea provides the streamed world state used to query block data. mctui also
maintains a packet-backed sidecar cache for chunk lighting and server time.
Packed block-light and sky-light arrays are combined with a time-of-day sky
curve to shade surfaces using the same information the server sends to clients.

### Raycasting and colour

Each frame casts camera rays through blocks using the Amanatides & Woo DDA
algorithm. Rays distinguish among a solid hit, confirmed open sky, and an
unloaded or unknown region. The last case renders dark rather than pretending
the missing data is empty space. A compact material palette, directional
shading, fog, and light values produce the final colours.

### Terminal output

Terminal rows are rendered with the Unicode upper-half-block character: its
foreground colour is the upper pixel and its background colour is the lower
pixel. That gives a 2× vertical effective pixel density while preserving full
24-bit ANSI colour.

## Development

```sh
cargo fmt
cargo test
cargo clippy --all-targets -- -D warnings
cargo build --release
```

`monitor`, `minimap`, and `ray` are deliberately small diagnostic modes that
make it easier to validate networking, streamed-world data, and raycasting
without running the full renderer.

## Scope and safety

mctui is a local-world visualiser and renderer. It can render, move, and look;
it does not mine, craft, manage inventory, fight, or automate gameplay. The
default local-only connection policy is intentional. If you use
`--allow-public-server`, follow that server's rules and obtain permission.

## Current limitations

- The material palette is intentionally compact, so some blocks share colours.
- The renderer can only display chunks the server has sent to the client.
- Nearby-entity markers are experimental and disabled by default while their
  world-state integration is being reworked.
