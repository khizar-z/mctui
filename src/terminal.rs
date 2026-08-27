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

use mctui::{
    BlockSource, Camera, Frame, RayResult, RenderConfig, Rgb, lighting::LightStore,
    navigation_minimap, raycast,
};

use crate::{
    hud::HudSnapshot,
    live::{
        ActionSender, BlockOverrides, EntitySnapshots, HudSnapshots, InventoryButton, PlayerAction,
    },
};

const MINIMAP_RADIUS: i32 = 7;
const MINIMAP_WIDTH: usize = (MINIMAP_RADIUS as usize * 2) + 1;
const MINIMAP_SIDEBAR_COLUMNS: usize = MINIMAP_WIDTH + 4; // gap, borders, and map cells
const MINIMAP_ROWS: usize = MINIMAP_WIDTH + 4; // title, borders, cells, legend
const MIN_RENDER_COLUMNS: usize = 20;
const HEADER_ROWS: usize = 3;
const HUD_ROWS: usize = 2;
const CHROME_ROWS: usize = HEADER_ROWS + HUD_ROWS;
const LOOK_STEP_DEGREES: f32 = 6.0;
const INTERACTION_REACH: f64 = 5.0;

pub(crate) struct TerminalResources {
    pub lighting: Arc<RwLock<LightStore>>,
    pub render_entities: bool,
    pub entity_snapshots: EntitySnapshots,
    pub hud_snapshots: HudSnapshots,
    pub block_overrides: BlockOverrides,
    pub actions: ActionSender,
}

/// Run the interactive renderer until the user presses `q` or Escape.
pub fn run(
    bot: Client,
    mut config: RenderConfig,
    target_fps: u16,
    resources: TerminalResources,
) -> Result<()> {
    let mut session = TerminalSession::enter()?;
    let layout = session.layout_for(config);
    config.width = layout.frame_width;
    config.height = layout.frame_height;
    let frame_period = Duration::from_secs_f64(1.0 / f64::from(target_fps.max(1)));
    let mut last_frame_at = Instant::now();
    let mut frames = 0_u32;
    let mut fps = 0.0_f32;
    let mut fps_window = Instant::now();
    let mut inventory_ui = InventoryUi::default();
    let mut interaction_target = None;

    loop {
        let frame_started = Instant::now();
        if !read_input(
            &bot,
            &resources.actions,
            &mut inventory_ui,
            interaction_target,
        )? {
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
        let entity_markers = if resources.render_entities {
            resources
                .entity_snapshots
                .read()
                .expect("entity snapshot lock poisoned")
                .clone()
        } else {
            Vec::new()
        };
        let hud = resources
            .hud_snapshots
            .read()
            .expect("HUD snapshot lock poisoned")
            .clone();
        let block_overrides = resources
            .block_overrides
            .read()
            .expect("block override lock poisoned")
            .clone();

        // Keep one read lock for the entire frame so every ray samples a
        // coherent snapshot of chunks that Azalea has already received.
        let (mut frame, minimap, target, next_interaction_target) = {
            let world = bot.world()?;
            let world = world.read();
            let lighting = resources
                .lighting
                .read()
                .expect("lighting cache lock poisoned");
            let live_world = crate::live::AzaleaWorld::new(&world, &lighting, &block_overrides);
            let frame = Frame::render_with_entities(&live_world, camera, config, &entity_markers);
            let minimap = layout.show_minimap.then(|| {
                navigation_minimap(
                    &live_world,
                    camera.origin.floor_to_block(),
                    camera.yaw_degrees,
                    MINIMAP_RADIUS,
                )
            });
            let (target, interaction_target) =
                target_readout(&live_world, camera, config.max_distance);
            (frame, minimap, target, interaction_target)
        };
        interaction_target = next_interaction_target;
        frame.draw_center_crosshair();

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
            DrawView {
                status: &format!(
                "mctui{}  {fps:>4.1} fps  {render_ms:>5.1} ms  pos {:.1} {:.1} {:.1}  yaw {:.1} pitch {:.1}",
                if config.procedural_textures { " +textures" } else { "" },
                position.x,
                position.y,
                position.z,
                direction.y_rot(),
                direction.x_rot(),
                ),
                target: &target,
                hud: &hud,
                minimap: minimap.as_deref(),
                inventory_ui: inventory_ui.open.then_some(&inventory_ui),
                text_columns: layout.text_columns,
            },
        )?;

        let remaining = frame_period.saturating_sub(last_frame_at.elapsed());
        if !remaining.is_zero() {
            thread::sleep(remaining);
        }
        last_frame_at = Instant::now();
    }

    Ok(())
}

fn target_readout(
    source: &impl BlockSource,
    camera: Camera,
    max_distance: f64,
) -> (String, Option<mctui::BlockPos>) {
    let (forward, _, _) = camera.basis();
    match raycast(source, camera.origin, forward, max_distance) {
        RayResult::Hit(hit) => {
            let light = source.light_at(hit.position);
            let face = hit
                .entered_face
                .map_or_else(|| "inside".to_owned(), |face| format!("{face:?}"));
            (
                format!(
                    "target: {} @ {}  {:.1}m  {face}  light {}/{}",
                    hit.block.id, hit.position, hit.distance, light.block, light.sky
                ),
                (hit.distance <= INTERACTION_REACH).then_some(hit.position),
            )
        }
        RayResult::Miss => (
            format!("target: sky (no block within {max_distance:.0}m)"),
            None,
        ),
        RayResult::Unloaded { position, distance } => (
            format!("target: unloaded at {position} after {distance:.1}m"),
            None,
        ),
    }
}

fn read_input(
    bot: &Client,
    actions: &ActionSender,
    inventory_ui: &mut InventoryUi,
    interaction_target: Option<mctui::BlockPos>,
) -> Result<bool> {
    while event::poll(Duration::ZERO)? {
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind == KeyEventKind::Release {
            // Most terminal emulators do not report key releases. If one does,
            // honour it by stopping the ongoing walk command.
            if !inventory_ui.open && matches!(key.code, KeyCode::Char('w' | 'a' | 's' | 'd')) {
                bot.walk(WalkDirection::None);
            }
            continue;
        }
        if key.kind != KeyEventKind::Press && key.kind != KeyEventKind::Repeat {
            continue;
        }
        if !apply_key(bot, actions, inventory_ui, interaction_target, key)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn apply_key(
    bot: &Client,
    actions: &ActionSender,
    inventory_ui: &mut InventoryUi,
    interaction_target: Option<mctui::BlockPos>,
    key: KeyEvent,
) -> Result<bool> {
    if inventory_ui.open {
        return apply_inventory_key(actions, inventory_ui, key);
    }

    let movement = match movement_for_key(key.code) {
        Some(movement) => Some(movement),
        None => match key.code {
            KeyCode::Char(' ') => {
                bot.jump();
                return Ok(true);
            }
            KeyCode::Char('x') => {
                bot.walk(WalkDirection::None);
                return Ok(true);
            }
            KeyCode::Char('e') => {
                inventory_ui.open = true;
                return Ok(true);
            }
            KeyCode::Char('f') => {
                queue_targeted_action(actions, interaction_target, PlayerAction::StartMining)?;
                return Ok(true);
            }
            KeyCode::Char('g') => {
                queue_targeted_action(actions, interaction_target, PlayerAction::UseTargetedBlock)?;
                return Ok(true);
            }
            KeyCode::Char(digit @ '1'..='9') => {
                let slot = digit as u8 - b'1';
                actions
                    .send(PlayerAction::SelectHotbarSlot(slot))
                    .map_err(|_| eyre::eyre!("client action loop stopped"))?;
                return Ok(true);
            }
            KeyCode::Esc | KeyCode::Char('q') => return Ok(false),
            _ => None,
        },
    };

    if let Some(movement) = movement {
        if key.modifiers.contains(KeyModifiers::SHIFT) && movement == WalkDirection::Forward {
            bot.sprint(SprintDirection::Forward);
        } else {
            bot.walk(movement);
        }
        return Ok(true);
    }

    if key.code == KeyCode::Char('c') {
        bot.set_crouching(!bot.crouching())?;
        return Ok(true);
    }

    let current = bot.direction()?;
    let Some((yaw, pitch)) = look_angles_after_key(current.y_rot(), current.x_rot(), key.code)
    else {
        return Ok(true);
    };
    bot.set_direction(yaw, pitch)?;
    Ok(true)
}

fn movement_for_key(key: KeyCode) -> Option<WalkDirection> {
    match key {
        KeyCode::Char('w') => Some(WalkDirection::Forward),
        KeyCode::Char('a') => Some(WalkDirection::Left),
        KeyCode::Char('s') => Some(WalkDirection::Backward),
        KeyCode::Char('d') => Some(WalkDirection::Right),
        _ => None,
    }
}

fn look_angles_after_key(yaw: f32, pitch: f32, key: KeyCode) -> Option<(f32, f32)> {
    match key {
        KeyCode::Left => Some((yaw - LOOK_STEP_DEGREES, pitch)),
        KeyCode::Right => Some((yaw + LOOK_STEP_DEGREES, pitch)),
        KeyCode::Up => Some((yaw, pitch - LOOK_STEP_DEGREES)),
        KeyCode::Down => Some((yaw, pitch + LOOK_STEP_DEGREES)),
        _ => None,
    }
}

fn queue_targeted_action(
    actions: &ActionSender,
    target: Option<mctui::BlockPos>,
    action: impl FnOnce(mctui::BlockPos) -> PlayerAction,
) -> Result<()> {
    let Some(target) = target else {
        return Ok(());
    };
    actions
        .send(action(target))
        .map_err(|_| eyre::eyre!("client action loop stopped"))
}

#[derive(Clone, Copy, Debug, Default)]
struct InventoryUi {
    open: bool,
    row: usize,
    column: usize,
    special_slot: Option<usize>,
}

impl InventoryUi {
    const ROWS: usize = 4;
    const COLUMNS: usize = 9;
    const SPECIAL_SLOTS: [usize; 10] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 45];

    fn selected_slot(self) -> usize {
        self.special_slot
            .unwrap_or(9 + self.row * Self::COLUMNS + self.column)
    }

    fn move_by(&mut self, row_delta: isize, column_delta: isize) {
        self.special_slot = None;
        self.row = (self.row as isize + row_delta).rem_euclid(Self::ROWS as isize) as usize;
        self.column =
            (self.column as isize + column_delta).rem_euclid(Self::COLUMNS as isize) as usize;
    }

    fn cycle_special_slot(&mut self, direction: isize) {
        let index = self
            .special_slot
            .and_then(|slot| {
                Self::SPECIAL_SLOTS
                    .iter()
                    .position(|candidate| *candidate == slot)
            })
            .map_or_else(
                || {
                    if direction.is_negative() {
                        Self::SPECIAL_SLOTS.len() - 1
                    } else {
                        0
                    }
                },
                |index| {
                    (index as isize + direction).rem_euclid(Self::SPECIAL_SLOTS.len() as isize)
                        as usize
                },
            );
        self.special_slot = Some(Self::SPECIAL_SLOTS[index]);
    }
}

fn apply_inventory_key(
    actions: &ActionSender,
    inventory_ui: &mut InventoryUi,
    key: KeyEvent,
) -> Result<bool> {
    match key.code {
        KeyCode::Esc | KeyCode::Char('e') => inventory_ui.open = false,
        KeyCode::Left => inventory_ui.move_by(0, -1),
        KeyCode::Right => inventory_ui.move_by(0, 1),
        KeyCode::Up => inventory_ui.move_by(-1, 0),
        KeyCode::Down => inventory_ui.move_by(1, 0),
        KeyCode::Tab => inventory_ui.cycle_special_slot(1),
        KeyCode::BackTab => inventory_ui.cycle_special_slot(-1),
        KeyCode::Enter => {
            let button = if key.modifiers.contains(KeyModifiers::SHIFT) {
                InventoryButton::QuickMove
            } else {
                InventoryButton::Left
            };
            actions
                .send(PlayerAction::InventoryClick {
                    slot: inventory_ui.selected_slot(),
                    button,
                })
                .map_err(|_| eyre::eyre!("client action loop stopped"))?;
        }
        KeyCode::Char('r') => {
            actions
                .send(PlayerAction::InventoryClick {
                    slot: inventory_ui.selected_slot(),
                    button: InventoryButton::Right,
                })
                .map_err(|_| eyre::eyre!("client action loop stopped"))?;
        }
        KeyCode::Char('q') => return Ok(false),
        _ => {}
    }
    Ok(true)
}

struct TerminalSession {
    output: io::Stdout,
    reports_key_releases: bool,
}

#[derive(Clone, Copy)]
struct RenderLayout {
    frame_width: usize,
    frame_height: usize,
    show_minimap: bool,
    /// Keep one terminal column unused: writing in the final column can
    /// trigger delayed wrapping in several terminal emulators.
    text_columns: usize,
}

struct DrawView<'a> {
    status: &'a str,
    target: &'a str,
    hud: &'a HudSnapshot,
    minimap: Option<&'a str>,
    inventory_ui: Option<&'a InventoryUi>,
    text_columns: usize,
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
        let (columns, rows) = terminal::size()
            .map(|(columns, rows)| (columns as usize, rows as usize))
            .unwrap_or((
                config.width + MINIMAP_SIDEBAR_COLUMNS,
                config.height + CHROME_ROWS,
            ));
        let text_columns = columns.saturating_sub(1).max(1);
        let frame_height = config.height.min(rows.saturating_sub(CHROME_ROWS).max(1));
        let show_minimap = frame_height >= MINIMAP_ROWS
            && text_columns >= MIN_RENDER_COLUMNS + MINIMAP_SIDEBAR_COLUMNS;
        let frame_width = if show_minimap {
            config
                .width
                .min(text_columns.saturating_sub(MINIMAP_SIDEBAR_COLUMNS))
        } else {
            config.width.min(text_columns)
        };
        RenderLayout {
            frame_width,
            frame_height,
            show_minimap,
            text_columns,
        }
    }

    fn draw(&mut self, frame: &Frame, view: DrawView<'_>) -> Result<()> {
        let minimap_rows: Vec<_> = view
            .minimap
            .map_or_else(Vec::new, |map| map.lines().collect());
        let mut bytes = String::with_capacity(frame.width * frame.sample_height * 22);
        bytes.push_str("\x1b[H");
        append_text_line(&mut bytes, view.status, view.text_columns, true);
        append_text_line(&mut bytes, view.target, view.text_columns, true);
        let controls = if view.inventory_ui.is_some() {
            "inventory: arrows select storage · Tab select equipment/craft · Enter pick/place · Shift+Enter move stack · R split/place one · E/Esc close · Q quit"
        } else if self.reports_key_releases {
            "WASD move/release stop · Shift+W sprint · arrows look · Space jump/swim · F break · G use/place · 1-9 hotbar · E inventory · Q quit"
        } else {
            "WASD move · Shift+W sprint · arrows look · Space jump/swim · F break · G use/place · 1-9 hotbar · E inventory · Q quit"
        };
        append_text_line(&mut bytes, controls, view.text_columns, true);

        if let Some(inventory_ui) = view.inventory_ui {
            append_inventory_overlay(
                &mut bytes,
                view.hud,
                inventory_ui,
                frame.sample_height / 2,
                view.text_columns,
            );
        } else {
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
        }
        append_text_line(&mut bytes, &view.hud.status_line(), view.text_columns, true);
        append_text_line(
            &mut bytes,
            &view.hud.hotbar_line(),
            view.text_columns,
            false,
        );
        self.output.write_all(bytes.as_bytes())?;
        self.output.flush()?;
        Ok(())
    }
}

fn append_inventory_overlay(
    output: &mut String,
    hud: &HudSnapshot,
    inventory_ui: &InventoryUi,
    rows: usize,
    text_columns: usize,
) {
    let selected_slot = inventory_ui.selected_slot();
    let cell = |slot| hud.inventory_cell(slot, slot == selected_slot);
    let row = |start| {
        (0..InventoryUi::COLUMNS)
            .map(|offset| cell(start + offset))
            .collect::<Vec<_>>()
            .join(" ")
    };
    let lines = [
        format!(
            "+---------------------- inventory ----------------------+  carried [{}]",
            hud.carried_cell()
        ),
        format!(
            "armor  {} {} {} {}   offhand {}",
            cell(1),
            cell(2),
            cell(3),
            cell(4),
            cell(45),
        ),
        format!(
            "craft  {} {}   {} {}   -> {}",
            cell(5),
            cell(6),
            cell(7),
            cell(8),
            cell(0),
        ),
        format!("main   {}", row(9)),
        format!("       {}", row(18)),
        format!("       {}", row(27)),
        format!("hotbar {}", row(36)),
        "+-------------------------------------------------------+".to_owned(),
        "server-synced player inventory · selected cell is >item<".to_owned(),
    ];

    for row in 0..rows {
        if let Some(line) = lines.get(row) {
            append_text_line(output, line, text_columns, true);
        } else {
            append_text_line(output, "", text_columns, true);
        }
    }
}

fn append_text_line(output: &mut String, line: &str, text_columns: usize, newline: bool) {
    output.push_str("\x1b[0m\x1b[2K");
    output.extend(line.chars().take(text_columns));
    if newline {
        output.push_str("\r\n");
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

    #[test]
    fn inventory_cursor_wraps_across_the_four_player_storage_rows() {
        let mut inventory = InventoryUi::default();
        inventory.move_by(-1, -1);

        assert_eq!(inventory.selected_slot(), 44);
        inventory.move_by(1, 1);
        assert_eq!(inventory.selected_slot(), 9);
    }

    #[test]
    fn inventory_cursor_cycles_special_equipment_and_crafting_slots() {
        let mut inventory = InventoryUi::default();
        inventory.cycle_special_slot(-1);
        assert_eq!(inventory.selected_slot(), 45);
        inventory.cycle_special_slot(1);
        assert_eq!(inventory.selected_slot(), 0);
        inventory.move_by(0, 0);
        assert_eq!(inventory.selected_slot(), 9);
    }

    #[test]
    fn inventory_overlay_marks_the_selected_server_slot() {
        let mut hud = HudSnapshot::default();
        hud.set_player_menu_slot(9, Some("minecraft:stone"), 64);
        let inventory = InventoryUi::default();
        let mut output = String::new();

        append_inventory_overlay(&mut output, &hud, &inventory, 9, 79);

        assert!(output.contains(">ST64<"));
        assert!(output.contains("server-synced player inventory"));
    }

    #[test]
    fn text_lines_are_clipped_before_the_terminal_wrap_column() {
        let mut output = String::new();
        append_text_line(&mut output, "12345", 4, false);

        assert_eq!(output, "\x1b[0m\x1b[2K1234");
    }

    #[test]
    fn look_arrows_follow_minecraft_yaw_direction() {
        assert_eq!(
            look_angles_after_key(0.0, 0.0, KeyCode::Left),
            Some((-6.0, 0.0))
        );
        assert_eq!(
            look_angles_after_key(0.0, 0.0, KeyCode::Right),
            Some((6.0, 0.0))
        );
        assert_eq!(
            look_angles_after_key(0.0, 0.0, KeyCode::Up),
            Some((0.0, -6.0))
        );
        assert_eq!(
            look_angles_after_key(0.0, 0.0, KeyCode::Down),
            Some((0.0, 6.0))
        );
    }

    #[test]
    fn wasd_lateral_keys_follow_the_player_perspective() {
        assert_eq!(
            movement_for_key(KeyCode::Char('a')),
            Some(WalkDirection::Left)
        );
        assert_eq!(
            movement_for_key(KeyCode::Char('d')),
            Some(WalkDirection::Right)
        );
    }
}
