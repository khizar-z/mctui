# mctui

`mctui` is a Rust client that connects to a live Minecraft Java Edition server
and renders the chunks streamed to its bot as a first-person 24-bit ANSI view.
It uses Azalea for protocol/world state and a custom Amanatides & Woo voxel
raycaster for every terminal sample.

This is intended for an offline-mode local vanilla/Paper server during
development. The program refuses non-loopback server addresses unless
`--allow-public-server` is supplied explicitly.

## Prerequisites

- Rust nightly (pinned in [rust-toolchain.toml](rust-toolchain.toml)); Azalea
  0.16 currently requires nightly because of its `simdnbt` dependency.
- A local Minecraft Java server compatible with Azalea 0.16 (`mc26.2`).
- A terminal with ANSI true-color support. macOS Terminal, iTerm2, WezTerm,
  Ghostty, and most modern Linux terminals work.

Start the offline-mode local server yourself, then use a distinct bot name:

```sh
cargo run --release -- --server 127.0.0.1:25565 --username terminal_view --mode monitor
```

The default server is `127.0.0.1:25565`, so it can be omitted.

## Phase validation commands

Run these in order against the local server.

| Phase | Command | Expected result |
| --- | --- | --- |
| 0–2 | `cargo run --release -- --mode monitor` | Logs connection, spawn, position/yaw, packet-backed `block`/`sky` light, and the current day factor. |
| 1 | `cargo run --release -- --mode minimap` | A continuously refreshed top-down slice with `@` at the bot's position; `?` is an unloaded chunk. |
| 2 | `cargo run --release -- --mode ray` | Prints the first block straight ahead, coordinates, entry face, and distance. Spot-check this in-game. |
| 3–7 | `cargo run --release -- --mode render --width 40 --height 20 --fps 12` | Low-resolution live first-person view with packet-backed lighting, day/night sky, water/glass transparency, and underwater overlay. |
| final | `cargo run --release -- --mode render` | `WASD` movement, `Shift+W` sprint, arrow-key look, `Space` jump/swim, `X` stop, `C` crouch, `Q` quit. |

The renderer requests an 8-chunk view distance by default. Adjust it and the
ray distance together if terrain appears as the dark unloaded-chunk color:

```sh
cargo run --release -- --mode render --view-distance 12 --distance 80
```

## Design notes

- One `▀` uses independent 24-bit foreground (top) and background (bottom)
  colors, producing two vertical ray samples per terminal character row.
- Each frame holds Azalea's world read lock while it raycasts, so all rays see
  a coherent received-chunk snapshot. Missing chunks are deliberately not
  treated as empty sky.
- Azalea intentionally does not retain light or clock state in its world
  object. mctui captures `Level Chunk with Light`, `Update Light`, and `Set
  Time` packets into a compact 2,048-byte-per-section sidecar cache. Its
  monitor output says `packet clock` once a real server clock update arrives.
- Block shading uses `max(block_light, sky_light × day_factor)` with a
  non-linear brightness curve; light fog and entry-face shading remain only
  as depth cues. The compact palette covers common terrain and falls back to
  neutral stone gray for unknown blocks.
- Water, glass, ice, and lava are alpha-composited through a maximum of four
  translucent layers per ray. Leaves remain opaque and green for readability.
  Water and lava also tint the whole image while the camera is inside them.
- Keyboard movement is latched because most terminal protocols do not expose
  reliable key-release events. Press `X` to stop; terminals that do send
  releases stop movement automatically. Space can be repeated to swim upward.

## Lighting validation on the local server

Run monitor mode while manually placing or moving the bot with the existing
controls. In open daylight it should report `block=0 sky=15`; beside a torch,
`block` should rise, while a dark interior should show both values low or zero.

If you have operator access on the local Paper server, use its console or an
ordinary player to run these manual checks, then watch the monitor line update:

```text
/time set 6000     # day factor near 1.00
/time set 18000    # day factor near 0.00
```

Finally, use render mode to look through a glass block or water at terrain
behind it, and enter/leave water to verify the full-frame blue overlay. These
checks do not add any gameplay automation to mctui.

## Safety

The client only renders, changes view direction, and sends ordinary movement
inputs. It has no mining, inventory, combat, or interaction automation.
For an authenticated connection, pass `--online` with a Microsoft account
cache key, and explicitly pass `--allow-public-server` only after confirming
that server's bot rules.
