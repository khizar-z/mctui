# mctui

**A live first-person Minecraft Java renderer for the terminal, written in Rust.**

<img width="800" height="519" alt="2026-08-2602-01-08-ezgif com-speed" src="https://github.com/user-attachments/assets/330568b6-eb5b-4371-8db9-a626832f2755" />

`mctui` connects to a local Minecraft Java server as a real client, consumes
its streamed world state, and renders a playable first-person view in a
true-colour ANSI terminal. It does not capture, proxy, or automate the
official game client: terrain is raycast directly from streamed voxel data
with a custom renderer.

The project explores the systems work behind a real-time text-mode renderer:
asynchronous networking, compact lighting data, voxel traversal, terminal I/O,
and safe synchronization between the game client and rendering thread.

## Highlights

- Streams local Minecraft Java world data through [Azalea](https://github.com/azalea-rs/azalea)
- Casts every view ray with a custom Amanatides & Woo DDA voxel raycaster
- Renders 24-bit colour at double vertical resolution with ANSI half blocks
- Uses packet-backed sky light, block light, and a smoothly advancing server clock
- Keeps lighting coherent when chunks stream, blocks change, and light updates arrive separately
- Handles water, glass, ice, and lava as translucent layers, including underwater overlays
- Offers opt-in procedural block textures: grain, grass sides, wood bands, ore flecks, leaf noise, and animated water
- Provides live target data, navigation minimap, health, hunger, XP, hotbar, and drowning HUDs
- Supports movement, sprinting, swimming, block breaking, block use/placement, and hotbar selection
- Includes a keyboard-driven, server-synchronised player inventory overlay
- Treats unloaded terrain conservatively instead of rendering unknown blocks as empty sky

```text
Minecraft packets → streamed blocks + light/clock sidecars → DDA raycaster → ANSI terminal frame
```

## Quick start

### Requirements

- Rust nightly; the included [`rust-toolchain.toml`](rust-toolchain.toml) selects the tested toolchain
- A local Minecraft Java server compatible with Minecraft `26.2`
- A true-colour ANSI terminal, such as Kitty, iTerm2, WezTerm, Ghostty, or a modern Linux terminal

Start the server separately, then run mctui from this repository. By default,
it connects in offline mode to `127.0.0.1:25565` as `mctui`.

```sh
cargo run --release -- --mode render
```

Use another name when a local player is already using `mctui`:

```sh
cargo run --release -- --username terminal_view --mode render
```

For a larger, farther-reaching view:

```sh
cargo run --release -- --mode render \
  --width 120 --height 36 --fps 10 \
  --view-distance 12 --distance 80
```

To try the experimental procedural material pass, add `--textures`:

```sh
cargo run --release -- --mode render --textures
```

It uses deterministic, world-anchored surface patterns rather than Minecraft
asset files or a resource pack. Without the flag, mctui keeps its original
compact flat-colour palette.

`--width` and `--height` control the rendered viewport. The minimap occupies
19 additional terminal columns, so the example above is most comfortable in a
terminal around 140 columns wide. Lower `--fps` or `--distance` if rendering
becomes expensive.

The client rejects non-loopback addresses unless `--allow-public-server` is
passed explicitly. Only connect it to servers that permit bots.

## Controls

| Input | Action |
| --- | --- |
| `W` / `A` / `S` / `D` | Move forward / left / backward / right |
| `Shift` + `W` | Sprint forward |
| Arrow keys | Look in 6° increments |
| `Space` | Jump or swim upward |
| `C` | Toggle crouch |
| `X` | Stop moving |
| `F` | Break the targeted block within normal reach |
| `G` | Use the targeted block or place the selected item against it |
| `1`–`9` | Select a hotbar slot |
| `E` | Open or close the player inventory |
| `Q` or `Esc` | Quit |

When the terminal reports Kitty keyboard-enhancement key releases, releasing a
movement key also stops the player. `X` is the portable stop control and works
in every supported terminal.

### Inventory controls

The inventory overlay reflects server-synchronised player slots and the carried
stack; it does not simulate an independent local inventory.

| Input | Action |
| --- | --- |
| Arrow keys | Select a main-inventory or hotbar slot |
| `Tab` / `Shift` + `Tab` | Cycle crafting, armor, and offhand slots |
| `Enter` | Pick up, place, merge, or swap a stack |
| `Shift` + `Enter` | Quick-move the selected stack |
| `R` | Right-click equivalent: split a stack or place one item |
| `E` or `Esc` | Close the overlay |

## Command line

```text
mctui [options]
```

| Flag | Default | Description |
| --- | --- | --- |
| `--server <HOST:PORT>` | `127.0.0.1:25565` | Minecraft server address |
| `--username <NAME>` | `mctui` | Offline name, or Microsoft-account email with `--online` |
| `--mode <MODE>` | `render` | `monitor`, `minimap`, `ray`, or `render` |
| `--width <COLS>` | `80` | Rendered viewport width in terminal columns |
| `--height <ROWS>` | `24` | Rendered viewport height in terminal rows |
| `--fps <FPS>` | `12` | Target interactive frame rate |
| `--distance <BLOCKS>` | `48` | Maximum DDA ray distance |
| `--fov <DEGREES>` | `75` | Horizontal field of view |
| `--view-distance <CHUNKS>` | `8` | Requested server chunk-view distance |
| `--entities` | off | Enable experimental nearby-entity markers |
| `--textures` | off | Enable experimental procedural block textures |
| `--online` | off | Use Microsoft authentication instead of offline mode |
| `--allow-public-server` | off | Permit a non-loopback server address |

The diagnostic modes are useful when working on one pipeline at a time:

| Mode | Purpose |
| --- | --- |
| `monitor` | Verifies connection, player position, light, and clock updates |
| `minimap` | Prints a top-down view of the loaded terrain layer |
| `ray` | Reports the first block reached by a forward camera ray |
| `render` | Starts the interactive first-person terminal renderer |

Run `cargo run --release -- --help` for the authoritative option list.

## Rendering pipeline

### Streamed world state

Azalea supplies the locally streamed chunks used for block queries. mctui keeps
small packet-backed sidecars for data the block world does not own: sky light,
block light, server time, HUD state, and short-lived block-update bridges. A
block update invalidates only its stale light sample until its corresponding
light packet arrives, avoiding dark artifacts without replacing the server as
the lighting authority.

### Voxel rendering

Each frame generates camera rays and advances them through the voxel grid with
DDA. A ray can hit an opaque block, collect a bounded number of translucent
layers, reach open sky, or enter an unloaded region. The last case is rendered
as unknown terrain rather than fabricated sky. Face orientation, light,
time-of-day, distance fog, and a compact material palette produce the final
pixel colour. With `--textures`, the final material colour receives a
deterministic face-local detail pass: stone grain, grass top and side layers,
wood rings and bands, ore flecks, leaf noise, and a time-animated water ripple.

### Terminal output

Each terminal cell uses the Unicode upper-half-block character. Its foreground
colour encodes one pixel and its background colour encodes the next, giving the
view a 2× vertical effective resolution while retaining full 24-bit ANSI
colour. Output is drawn in an alternate screen and deliberately avoids the
last terminal column, which prevents soft-wrap artifacts in common emulators.

## Development

```sh
cargo fmt
cargo test
cargo clippy --all-targets -- -D warnings
cargo build --release
```

The small diagnostic modes make it possible to validate networking, world
sampling, ray traversal, and terminal rendering independently without running
the full interactive renderer.

## Scope and limitations

mctui is a local-world renderer with direct, player-triggered interactions. It
does not craft, fight, pathfind, or automate gameplay. The local-only default
is intentional.

- The material palette is compact, so some block types share colours.
- The renderer can only show chunks and lighting the server has provided.
- Nearby-entity markers remain experimental and are disabled by default.
