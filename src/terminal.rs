//! ANSI terminal presentation and input handling.

use std::{
    io::{self, Write},
    sync::{Arc, RwLock},
    thread,
    time::{Duration, Instant},
};

use azalea::{Client, SprintDirection, WalkDirection};
use crossterm::{
    cursor::{Hide, Show},
    event::{
        self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags,
        PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen},
};
use eyre::Result;

use mctui::{Camera, Frame, RenderConfig, Rgb, lighting::LightStore, navigation_minimap};

const MINIMAP_RADIUS: i32 = 7;
const MINIMAP_WIDTH: usize = (MINIMAP_RADIUS as usize * 2) + 1;
const MINIMAP_SIDEBAR_COLUMNS: usize = MINIMAP_WIDTH + 4; // gap, borders, and map cells
const MINIMAP_ROWS: usize = MINIMAP_WIDTH + 4; // title, borders, cells, legend
const MIN_RENDER_COLUMNS: usize = 20;

/// Run the interactive renderer until the user presses `q` or Escape.
pub fn run(
    bot: Client,
    mut config: RenderConfig,
    target_fps: u16,
    lighting: Arc<RwLock<LightStore>>,
) -> Result<()> {
    let mut session = TerminalSession::enter()?;
    let layout = session.layout_for(config);
    config.width = layout.frame_width;
    let frame_period = Duration::from_secs_f64(1.0 / f64::from(target_fps.max(1)));
    let mut last_frame_at = Instant::now();
    let mut frames = 0_u32;
    let mut fps = 0.0_f32;
    let mut fps_window = Instant::now();

    loop {
        let frame_started = Instant::now();
        if !read_input(&bot)? {
            bot.exit();
            break;
        }

        let (position, direction) = match (bot.eye_position(), bot.direction()) {
            (Ok(position), Ok(direction)) => (position, direction),
            _ => {
                thread::sleep(Duration::from_millis(10));
                continue;
            }
        };
        let camera = Camera {
            origin: mctui::Vec3::new(position.x, position.y, position.z),
            yaw_degrees: direction.y_rot(),
            pitch_degrees: direction.x_rot(),
        };

        // Keep one read lock for the entire frame so every ray samples a
        // coherent snapshot of chunks that Azalea has already received.
        let (frame, minimap) = {
            let world = bot.world()?;
            let world = world.read();
            let lighting = lighting.read().expect("lighting cache lock poisoned");
            let live_world = crate::live::AzaleaWorld::new(&world, &lighting);
            let frame = Frame::render(&live_world, camera, config);
            let minimap = layout.show_minimap.then(|| {
                navigation_minimap(
                    &live_world,
                    camera.origin.floor_to_block(),
                    camera.yaw_degrees,
                    MINIMAP_RADIUS,
                )
            });
            (frame, minimap)
        };

        frames += 1;
        let elapsed = fps_window.elapsed();
        if elapsed >= Duration::from_secs(1) {
            fps = frames as f32 / elapsed.as_secs_f32();
            frames = 0;
            fps_window = Instant::now();
        }
        let render_ms = frame_started.elapsed().as_secs_f32() * 1_000.0;
        session.draw(
            &frame,
            &format!(
                "mctui  {fps:>4.1} fps  {render_ms:>5.1} ms  pos {:.1} {:.1} {:.1}  yaw {:.1} pitch {:.1}",
                position.x,
                position.y,
                position.z,
                direction.y_rot(),
                direction.x_rot(),
            ),
            minimap.as_deref(),
        )?;

        let remaining = frame_period.saturating_sub(last_frame_at.elapsed());
        if !remaining.is_zero() {
            thread::sleep(remaining);
        }
        last_frame_at = Instant::now();
    }

    Ok(())
}

fn read_input(bot: &Client) -> Result<bool> {
    while event::poll(Duration::ZERO)? {
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind == KeyEventKind::Release {
            // Most terminal emulators do not report key releases. If one does,
            // honour it by stopping the ongoing walk command.
            if matches!(key.code, KeyCode::Char('w' | 'a' | 's' | 'd')) {
                bot.walk(WalkDirection::None);
            }
            continue;
        }
        if key.kind != KeyEventKind::Press && key.kind != KeyEventKind::Repeat {
            continue;
        }
        if !apply_key(bot, key)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn apply_key(bot: &Client, key: KeyEvent) -> Result<bool> {
    let movement = match key.code {
        KeyCode::Char('w') => Some(WalkDirection::Forward),
        KeyCode::Char('s') => Some(WalkDirection::Backward),
        // Azalea's lateral walk directions are inverted relative to the
        // rendered camera's handedness, so map the familiar WASD bindings
        // accordingly.
        KeyCode::Char('a') => Some(WalkDirection::Right),
        KeyCode::Char('d') => Some(WalkDirection::Left),
        KeyCode::Char(' ') => {
            bot.jump();
            return Ok(true);
        }
        KeyCode::Char('x') => {
            bot.walk(WalkDirection::None);
            return Ok(true);
        }
        KeyCode::Esc | KeyCode::Char('q') => return Ok(false),
        _ => None,
    };

    if let Some(movement) = movement {
        if key.modifiers.contains(KeyModifiers::SHIFT) && movement == WalkDirection::Forward {
            bot.sprint(SprintDirection::Forward);
        } else {
            bot.walk(movement);
        }
        return Ok(true);
    }

    let look_step = 4.0;
    let current = bot.direction()?;
    let (yaw, pitch) = match key.code {
        // Azalea's yaw sign is the opposite of the intuitive terminal
        // left/right direction in this camera projection.
        KeyCode::Left => (current.y_rot() + look_step, current.x_rot()),
        KeyCode::Right => (current.y_rot() - look_step, current.x_rot()),
        KeyCode::Up => (current.y_rot(), current.x_rot() - look_step),
        KeyCode::Down => (current.y_rot(), current.x_rot() + look_step),
        KeyCode::Char('c') => {
            bot.set_crouching(!bot.crouching())?;
            return Ok(true);
        }
        _ => return Ok(true),
    };
    bot.set_direction(yaw, pitch)?;
    Ok(true)
}

struct TerminalSession {
    output: io::Stdout,
    reports_key_releases: bool,
}

#[derive(Clone, Copy)]
struct RenderLayout {
    frame_width: usize,
    show_minimap: bool,
}

impl TerminalSession {
    fn enter() -> Result<Self> {
        terminal::enable_raw_mode()?;
        let supports_key_releases = matches!(terminal::supports_keyboard_enhancement(), Ok(true));
        let mut session = Self {
            output: io::stdout(),
            reports_key_releases: false,
        };
        execute!(
            session.output,
            EnterAlternateScreen,
            Hide,
            Clear(ClearType::All)
        )?;

        if supports_key_releases {
            // Kitty's legacy mode only sends presses. The all-keys flag is
            // required for printable movement keys such as WASD, while the
            // event-type flag adds the release events read_input already uses.
            execute!(
                session.output,
                PushKeyboardEnhancementFlags(
                    KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                        | KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES
                        | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
                )
            )?;
            session.reports_key_releases = true;
        }

        Ok(session)
    }

    fn layout_for(&self, config: RenderConfig) -> RenderLayout {
        let columns = terminal::size()
            .map(|(columns, _)| columns as usize)
            .unwrap_or(config.width + MINIMAP_SIDEBAR_COLUMNS);
        let show_minimap = config.height >= MINIMAP_ROWS
            && columns >= MIN_RENDER_COLUMNS + MINIMAP_SIDEBAR_COLUMNS;
        let frame_width = if show_minimap {
            config
                .width
                .min(columns.saturating_sub(MINIMAP_SIDEBAR_COLUMNS))
        } else {
            config.width.min(columns.max(1))
        };
        RenderLayout {
            frame_width,
            show_minimap,
        }
    }

    fn draw(&mut self, frame: &Frame, status: &str, minimap: Option<&str>) -> Result<()> {
        let minimap_rows: Vec<_> = minimap.map_or_else(Vec::new, |map| map.lines().collect());
        let mut bytes = String::with_capacity(frame.width * frame.sample_height * 22);
        bytes.push_str("\x1b[H\x1b[0m\x1b[2K");
        bytes.push_str(status);
        bytes.push_str("\r\n\x1b[2K");
        bytes.push_str(if self.reports_key_releases {
            "WASD move/release stop · Shift+W sprint · arrows look · Space jump/swim · X stop · C crouch · Q quit"
        } else {
            "WASD move · Shift+W sprint · arrows look · Space jump/swim · X stop · C crouch · Q quit"
        });
        bytes.push_str("\x1b[0m\r\n");

        for row in 0..(frame.sample_height / 2) {
            for column in 0..frame.width {
                append_half_block(
                    &mut bytes,
                    frame.pixel(column, row * 2),
                    frame.pixel(column, row * 2 + 1),
                );
            }
            append_minimap_sidebar(&mut bytes, row, &minimap_rows);
            bytes.push_str("\x1b[0m\x1b[K\r\n");
        }
        self.output.write_all(bytes.as_bytes())?;
        self.output.flush()?;
        Ok(())
    }
}

fn append_minimap_sidebar(output: &mut String, row: usize, minimap_rows: &[&str]) {
    if minimap_rows.is_empty() {
        return;
    }

    output.push_str("\x1b[0m  ");
    match row {
        0 => output.push_str("minimap (N up)"),
        1 => output.push_str("+---------------+"),
        map_row @ 2..=16 => {
            output.push('|');
            output.push_str(minimap_rows[map_row - 2]);
            output.push('|');
        }
        17 => output.push_str("+---------------+"),
        18 => output.push_str("? = unloaded"),
        _ => {}
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        if self.reports_key_releases {
            let _ = execute!(self.output, PopKeyboardEnhancementFlags);
        }
        let _ = execute!(self.output, Show, LeaveAlternateScreen);
        let _ = terminal::disable_raw_mode();
    }
}

fn append_half_block(output: &mut String, top: Rgb, bottom: Rgb) {
    use std::fmt::Write;

    let _ = write!(
        output,
        "\x1b[38;2;{};{};{}m\x1b[48;2;{};{};{}m▀",
        top.r, top.g, top.b, bottom.r, bottom.g, bottom.b
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_block_uses_independent_foreground_and_background() {
        let mut encoded = String::new();
        append_half_block(&mut encoded, Rgb::new(1, 2, 3), Rgb::new(4, 5, 6));
        assert_eq!(encoded, "\x1b[38;2;1;2;3m\x1b[48;2;4;5;6m▀");
    }
}
