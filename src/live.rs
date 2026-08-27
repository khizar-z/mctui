//! Azalea integration and the phase-oriented runtime modes.

use std::{
    collections::HashMap,
    env, fmt,
    net::IpAddr,
    str::FromStr,
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, Sender},
    },
};

use azalea::{
    Client, ClientInformation, Event,
    block::fluid_state::FluidKind,
    core::data_registry::DataRegistryWithKey,
    ecs::query::Without,
    entity::{
        EntityKindComponent, FluidOnEyes, LocalEntity, Position, dimensions::EntityDimensions,
        metadata::AirSupply,
    },
    prelude::{Account, Component, bevy_ecs},
    registry::data::WorldClockKey,
    world::WorldName,
};
use eyre::{Result, bail};

use crate::hud::HudSnapshot;

use mctui::{
    Block, BlockPos, BlockSource, Camera, EntityCategory, EntityMarker, LightLevels, RayResult,
    RenderConfig, Vec3, Voxel,
    lighting::{LightStore, PacketLightData, WorldTime},
    minimap, raycast,
};

/// The live program mode. Each mode makes a project phase independently
/// verifiable against a local server.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mode {
    Monitor,
    Minimap,
    Ray,
    Render,
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Monitor => "monitor",
            Self::Minimap => "minimap",
            Self::Ray => "ray",
            Self::Render => "render",
        })
    }
}

impl FromStr for Mode {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value {
            "monitor" => Ok(Self::Monitor),
            "minimap" => Ok(Self::Minimap),
            "ray" => Ok(Self::Ray),
            "render" => Ok(Self::Render),
            _ => Err(format!(
                "unknown mode {value:?}; use monitor, minimap, ray, or render"
            )),
        }
    }
}

/// Command-line configuration for one local-server session.
#[derive(Clone, Debug)]
pub struct LiveConfig {
    pub server: String,
    pub username: String,
    pub online_account: bool,
    pub allow_public_server: bool,
    pub mode: Mode,
    pub render: RenderConfig,
    pub target_fps: u16,
    pub view_distance: u8,
    pub render_entities: bool,
}

impl Default for LiveConfig {
    fn default() -> Self {
        Self {
            server: "127.0.0.1:25565".to_owned(),
            username: "mctui".to_owned(),
            online_account: false,
            allow_public_server: false,
            mode: Mode::Render,
            render: RenderConfig::default(),
            target_fps: 12,
            view_distance: 8,
            render_entities: false,
        }
    }
}

impl LiveConfig {
    pub fn parse_env() -> Result<Self> {
        let mut config = Self::default();
        let mut args = env::args().skip(1);
        while let Some(argument) = args.next() {
            match argument.as_str() {
                "--server" => config.server = next_argument("--server", &mut args)?,
                "--username" => config.username = next_argument("--username", &mut args)?,
                "--mode" => {
                    config.mode = next_argument("--mode", &mut args)?
                        .parse()
                        .map_err(eyre::Report::msg)?
                }
                "--width" => {
                    config.render.width =
                        parse_flag("--width", next_argument("--width", &mut args)?)?
                }
                "--height" => {
                    config.render.height =
                        parse_flag("--height", next_argument("--height", &mut args)?)?
                }
                "--fps" => {
                    config.target_fps = parse_flag("--fps", next_argument("--fps", &mut args)?)?
                }
                "--distance" => {
                    config.render.max_distance =
                        parse_flag("--distance", next_argument("--distance", &mut args)?)?
                }
                "--fov" => {
                    config.render.horizontal_fov_degrees =
                        parse_flag("--fov", next_argument("--fov", &mut args)?)?
                }
                "--view-distance" => {
                    config.view_distance = parse_flag(
                        "--view-distance",
                        next_argument("--view-distance", &mut args)?,
                    )?
                }
                "--entities" => config.render_entities = true,
                "--online" => config.online_account = true,
                "--allow-public-server" => config.allow_public_server = true,
                "--help" | "-h" => {
                    print_help();
                    std::process::exit(0);
                }
                _ => bail!("unknown argument {argument:?}; use --help"),
            }
        }
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        if !is_local_server(&self.server) && !self.allow_public_server {
            bail!(
                "refusing to connect to {:?}. Test only against a local server by default; \
                 pass --allow-public-server only after confirming that server permits bots.",
                self.server
            );
        }
        if !(1..=240).contains(&self.render.width) || !(1..=80).contains(&self.render.height) {
            bail!("--width must be 1..=240 and --height must be 1..=80");
        }
        if !(30.0..=140.0).contains(&self.render.horizontal_fov_degrees) {
            bail!("--fov must be between 30 and 140 degrees");
        }
        if !(1.0..=256.0).contains(&self.render.max_distance) {
            bail!("--distance must be between 1 and 256 blocks");
        }
        Ok(())
    }
}

fn next_argument(flag: &str, args: &mut impl Iterator<Item = String>) -> Result<String> {
    args.next()
        .ok_or_else(|| eyre::eyre!("{flag} requires a value"))
}

fn parse_flag<T: FromStr>(flag: &str, value: String) -> Result<T>
where
    T::Err: fmt::Display + Send + Sync + 'static,
{
    value
        .parse()
        .map_err(|error| eyre::eyre!("invalid {flag} value {value:?}: {error}"))
}

fn is_local_server(address: &str) -> bool {
    let host = address
        .trim()
        .strip_prefix('[')
        .and_then(|value| value.split_once(']').map(|(host, _)| host))
        .or_else(|| address.rsplit_once(':').map(|(host, _)| host))
        .unwrap_or(address)
        .trim_matches(['[', ']']);
    if matches!(host, "localhost" | "0.0.0.0" | "::1") {
        return true;
    }
    host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

fn print_help() {
    println!(
        "mctui — a live Minecraft Java terminal renderer\n\n\
Usage: mctui [options]\n\n\
  --server ADDRESS        Local server address (default: 127.0.0.1:25565)\n\
  --username NAME         Offline username, or Microsoft account email with --online\n\
  --mode MODE             monitor | minimap | ray | render (default: render)\n\
  --width CELLS           Terminal frame width (default: 80)\n\
  --height ROWS           Terminal frame character height (default: 24)\n\
  --fps N                 Renderer target FPS (default: 12)\n\
  --distance BLOCKS       DDA render distance (default: 48)\n\
  --fov DEGREES           Horizontal FOV (default: 75)\n\
  --view-distance CHUNKS  Server chunk request distance (default: 8)\n\
  --entities              Enable experimental nearby-entity markers\n\
  --online                Use Microsoft authentication instead of offline mode\n\
  --allow-public-server   Required before any non-local connection\n\n\
Modes: monitor proves bot/position events; minimap proves block access; ray\n\
proves the DDA ray; render starts the interactive half-block view."
    );
}

/// Live adapter from Azalea's streamed chunk store to the generic raycaster.
pub struct AzaleaWorld<'a> {
    world: &'a azalea::world::World,
    lighting: &'a LightStore,
    block_overrides: &'a HashMap<BlockPos, BlockOverride>,
}

impl<'a> AzaleaWorld<'a> {
    pub fn new(
        world: &'a azalea::world::World,
        lighting: &'a LightStore,
        block_overrides: &'a HashMap<BlockPos, BlockOverride>,
    ) -> Self {
        Self {
            world,
            lighting,
            block_overrides,
        }
    }
}

impl BlockSource for AzaleaWorld<'_> {
    fn voxel_at(&self, position: BlockPos) -> Voxel {
        if let Some(block) = self.block_overrides.get(&position) {
            return match block {
                BlockOverride::Air => Voxel::Air,
                BlockOverride::Solid(id) => Voxel::Solid(Block { id }),
            };
        }
        let position = azalea::BlockPos::new(position.x, position.y, position.z);
        let Some(state) = self.world.get_block_state(position) else {
            return Voxel::Unloaded;
        };
        match block_override_from_id(state.to_trait().id()) {
            BlockOverride::Air => Voxel::Air,
            BlockOverride::Solid(id) => Voxel::Solid(Block { id }),
        }
    }

    fn light_at(&self, position: BlockPos) -> LightLevels {
        // A packet is normally received before the chunk is rendered. Retain
        // a bright fallback during that tiny startup window rather than
        // incorrectly making freshly streamed terrain pitch black.
        self.lighting
            .light_at(position)
            .unwrap_or(LightLevels::FULL_SKY)
    }

    fn day_factor(&self) -> f32 {
        self.lighting.day_factor()
    }
}

/// Shared, immutable-by-convention entity data for the terminal renderer.
///
/// The renderer receives a cloned vector from this store and never touches
/// Azalea's ECS. That separation prevents a slow terminal frame from blocking
/// client-world updates.
pub(crate) type EntitySnapshots = Arc<RwLock<Vec<EntityMarker>>>;

/// Shared packet-backed player HUD state for the terminal renderer.
pub(crate) type HudSnapshots = Arc<RwLock<HudSnapshot>>;

/// The latest authoritative block states received after a chunk was streamed.
///
/// Azalea applies these updates to its shared world on the next ECS update;
/// this small sidecar closes that transient gap for the renderer without
/// retaining a world or ECS lock on the terminal thread.
pub(crate) type BlockOverrides = Arc<RwLock<HashMap<BlockPos, BlockOverride>>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BlockOverride {
    Air,
    Solid(&'static str),
}

/// Commands produced by terminal input and executed from Azalea's event loop.
///
/// Rendering never takes the ECS lock for these actions. That keeps input
/// responsive without reviving the terminal-thread ECS contention that caused
/// entity rendering to freeze the client.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PlayerAction {
    StartMining(BlockPos),
    UseTargetedBlock(BlockPos),
    SelectHotbarSlot(u8),
    InventoryClick {
        slot: usize,
        button: InventoryButton,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InventoryButton {
    Left,
    Right,
    QuickMove,
}

pub(crate) type ActionSender = Sender<PlayerAction>;
type ActionReceiver = Arc<Mutex<Receiver<PlayerAction>>>;

/// Capture nearby streamed entities for the renderer.
///
/// This runs from Azalea's event handler instead of the terminal thread. It
/// takes the ECS lock at most once and uses `try_write` so an entity refresh
/// is skipped, rather than ever blocking the client loop, when the ECS is
/// momentarily busy.
fn refresh_entity_snapshots(bot: &Client, snapshots: &EntitySnapshots) {
    const MAX_MARKERS: usize = 24;

    let Some(mut ecs) = bot.ecs.try_write() else {
        return;
    };

    let Some(world_name) = ecs.get::<WorldName>(bot.entity).cloned() else {
        return;
    };
    let Some(player_position) = ecs
        .get::<Position>(bot.entity)
        .map(|position| Vec3::new(position.x, position.y, position.z))
    else {
        return;
    };

    let mut query = ecs.query_filtered::<(
        &WorldName,
        &Position,
        &EntityDimensions,
        &EntityKindComponent,
    ), Without<LocalEntity>>();
    let mut markers = query
        .iter(&ecs)
        .filter(|(entity_world, ..)| *entity_world == &world_name)
        .filter_map(|(_, position, dimensions, kind)| {
            (dimensions.width > 0.0 && dimensions.height > 0.0).then_some(EntityMarker {
                position: Vec3::new(position.x, position.y, position.z),
                width: f64::from(dimensions.width),
                height: f64::from(dimensions.height),
                category: entity_category(**kind),
            })
        })
        .collect::<Vec<_>>();
    markers.sort_by(|left, right| {
        let left_offset = left.position - player_position;
        let right_offset = right.position - player_position;
        left_offset
            .dot(left_offset)
            .total_cmp(&right_offset.dot(right_offset))
    });
    markers.truncate(MAX_MARKERS);

    // Release Azalea's ECS before publishing. Rendering only locks the small
    // Vec long enough to clone it, so these two locks can never form a cycle.
    drop(query);
    drop(ecs);
    *snapshots.write().expect("entity snapshot lock poisoned") = markers;
}

fn entity_category(kind: azalea::registry::builtin::EntityKind) -> EntityCategory {
    use azalea::registry::builtin::EntityKind;

    match kind {
        EntityKind::Player => EntityCategory::Player,
        EntityKind::Blaze
        | EntityKind::Bogged
        | EntityKind::Breeze
        | EntityKind::CaveSpider
        | EntityKind::Creeper
        | EntityKind::Creaking
        | EntityKind::Drowned
        | EntityKind::ElderGuardian
        | EntityKind::Enderman
        | EntityKind::Endermite
        | EntityKind::Evoker
        | EntityKind::Ghast
        | EntityKind::Guardian
        | EntityKind::Hoglin
        | EntityKind::Husk
        | EntityKind::Illusioner
        | EntityKind::MagmaCube
        | EntityKind::Parched
        | EntityKind::Phantom
        | EntityKind::Piglin
        | EntityKind::PiglinBrute
        | EntityKind::Pillager
        | EntityKind::Ravager
        | EntityKind::Shulker
        | EntityKind::Silverfish
        | EntityKind::Skeleton
        | EntityKind::Slime
        | EntityKind::Spider
        | EntityKind::Stray
        | EntityKind::Vex
        | EntityKind::Vindicator
        | EntityKind::Warden
        | EntityKind::Witch
        | EntityKind::Wither
        | EntityKind::WitherSkeleton
        | EntityKind::Zoglin
        | EntityKind::Zombie
        | EntityKind::ZombieVillager
        | EntityKind::ZombifiedPiglin => EntityCategory::Hostile,
        _ => EntityCategory::Passive,
    }
}

#[derive(Clone, Component)]
pub struct AppState {
    config: Arc<LiveConfig>,
    renderer_started: Arc<AtomicBool>,
    lighting: Arc<RwLock<LightStore>>,
    entity_snapshots: EntitySnapshots,
    hud: HudSnapshots,
    block_overrides: BlockOverrides,
    action_sender: ActionSender,
    action_receiver: ActionReceiver,
}

impl AppState {
    pub fn new(config: LiveConfig) -> Self {
        let (action_sender, action_receiver) = std::sync::mpsc::channel();
        Self {
            config: Arc::new(config),
            renderer_started: Arc::new(AtomicBool::new(false)),
            lighting: Arc::new(RwLock::new(LightStore::default())),
            entity_snapshots: Arc::new(RwLock::new(Vec::new())),
            hud: Arc::new(RwLock::new(HudSnapshot::default())),
            block_overrides: Arc::new(RwLock::new(HashMap::new())),
            action_sender,
            action_receiver: Arc::new(Mutex::new(action_receiver)),
        }
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new(LiveConfig::default())
    }
}

/// Connect a bot and dispatch the selected phase mode.
pub async fn start(config: LiveConfig) -> Result<()> {
    let account = if config.online_account {
        Account::microsoft(&config.username).await?
    } else {
        Account::offline(&config.username)
    };
    println!(
        "Connecting {} as {} in {} mode...",
        config.server, config.username, config.mode
    );
    azalea::ClientBuilder::new()
        .set_handler(handle_event)
        .set_state(AppState::new(config.clone()))
        .reconnect_after(None)
        .start(account, config.server)
        .await;
    Ok(())
}

async fn handle_event(bot: Client, event: Event, state: AppState) -> Result<()> {
    match event {
        Event::Init => {
            bot.set_client_information(ClientInformation {
                view_distance: state.config.view_distance,
                ..Default::default()
            })?;
            println!(
                "Connected; requesting {} chunks of view distance.",
                state.config.view_distance
            );
        }
        Event::Login => {
            state
                .lighting
                .write()
                .expect("lighting cache lock poisoned")
                .clear();
            state
                .entity_snapshots
                .write()
                .expect("entity snapshot lock poisoned")
                .clear();
            state
                .block_overrides
                .write()
                .expect("block override lock poisoned")
                .clear();
            *state.hud.write().expect("HUD snapshot lock poisoned") = HudSnapshot::default();
            println!("Login accepted; waiting for spawn and chunks...");
        }
        Event::Spawn => {
            println!("Spawned at {}", format_position(bot.position()?));
            if state.config.mode == Mode::Render
                && !state.renderer_started.swap(true, Ordering::AcqRel)
            {
                let render_bot = bot.clone();
                let config = state.config.clone();
                let lighting = state.lighting.clone();
                let entity_snapshots = state.entity_snapshots.clone();
                let hud = state.hud.clone();
                let block_overrides = state.block_overrides.clone();
                let action_sender = state.action_sender.clone();
                std::thread::spawn(move || {
                    if let Err(error) = crate::terminal::run(
                        render_bot.clone(),
                        config.render,
                        config.target_fps,
                        crate::terminal::TerminalResources {
                            lighting,
                            render_entities: config.render_entities,
                            entity_snapshots,
                            hud_snapshots: hud,
                            block_overrides,
                            actions: action_sender,
                        },
                    ) {
                        eprintln!("terminal renderer stopped: {error:?}");
                        render_bot.exit();
                    }
                });
            }
        }
        Event::Packet(packet) => capture_protocol_data(
            &bot,
            &state.lighting,
            &state.hud,
            &state.block_overrides,
            &packet,
        ),
        Event::Chat(chat) => println!("chat: {}", chat.message().to_ansi()),
        Event::Tick => {
            drain_player_actions(&bot, &state.hud, &state.action_receiver);
            reconcile_block_overrides(&bot, &state.block_overrides);
            refresh_drowning_indicator(&bot, &state.hud);
            if state.config.mode == Mode::Render
                && state.config.render_entities
                && bot.ticks_connected().is_multiple_of(5)
            {
                refresh_entity_snapshots(&bot, &state.entity_snapshots);
            }

            match state.config.mode {
                Mode::Monitor if bot.ticks_connected().is_multiple_of(10) => {
                    let position = bot.position()?;
                    let direction = bot.direction()?;
                    let light_position = position_to_block(position);
                    let lighting = state.lighting.read().expect("lighting cache lock poisoned");
                    let light = lighting.light_at(light_position);
                    println!(
                        "position {}  yaw {:.1} pitch {:.1}  light {}  day {:.2} ({})",
                        format_position(position),
                        direction.y_rot(),
                        direction.x_rot(),
                        light.map_or_else(
                            || "pending".to_owned(),
                            |levels| format!("block={} sky={}", levels.block, levels.sky)
                        ),
                        lighting.day_factor(),
                        if lighting.has_received_time() {
                            "packet clock"
                        } else {
                            "initial noon fallback"
                        },
                    );
                }
                Mode::Minimap if bot.ticks_connected().is_multiple_of(4) => {
                    print_live_minimap(&bot, &state.lighting, &state.block_overrides)?
                }
                Mode::Ray if bot.ticks_connected().is_multiple_of(5) => print_forward_hit(
                    &bot,
                    state.config.render.max_distance,
                    &state.lighting,
                    &state.block_overrides,
                )?,
                _ => {}
            }
        }
        Event::Disconnect(reason) => {
            eprintln!("Disconnected: {reason:?}");
            bot.exit();
        }
        Event::ConnectionFailed(error) => {
            eprintln!("Connection failed: {error:?}");
            bot.exit();
        }
        _ => {}
    }
    Ok(())
}

fn capture_protocol_data(
    bot: &Client,
    store: &Arc<RwLock<LightStore>>,
    hud: &HudSnapshots,
    block_overrides: &BlockOverrides,
    packet: &azalea::protocol::packets::game::ClientboundGamePacket,
) {
    use azalea::protocol::packets::game::ClientboundGamePacket;

    match packet {
        ClientboundGamePacket::SetTime(packet) => {
            let overworld_time = packet.clock_updates.iter().find_map(|(clock_id, clock)| {
                matches!(
                    bot.with_registry_holder(|registries| clock_id.key_owned(registries)),
                    Ok(Some(WorldClockKey::Overworld))
                )
                .then_some(WorldTime {
                    total_ticks: clock.total_ticks,
                    partial_tick: clock.partial_tick,
                    rate: clock.rate,
                })
            });
            let mut store = store.write().expect("lighting cache lock poisoned");
            if let Some(time) = overworld_time {
                store.set_time(time);
            } else if !store.has_received_time() {
                // Some servers send a clock-less first update. It is safe as
                // an initial value, but must never replace a known Overworld
                // clock with data from another dimension.
                store.set_time(WorldTime {
                    total_ticks: packet.game_time,
                    partial_tick: 0.0,
                    rate: 1.0,
                });
            }
        }
        ClientboundGamePacket::ForgetLevelChunk(packet) => {
            store
                .write()
                .expect("lighting cache lock poisoned")
                .remove_chunk(packet.pos.x, packet.pos.z);
            remove_chunk_block_overrides(block_overrides, packet.pos.x, packet.pos.z);
        }
        ClientboundGamePacket::LightUpdate(packet) => {
            if let Some(min_y) = world_min_y(bot) {
                apply_light_packet(store, packet.x, packet.z, min_y, &packet.light_data);
            }
        }
        ClientboundGamePacket::LevelChunkWithLight(packet) => {
            if let Some(min_y) = world_min_y(bot) {
                apply_light_packet(store, packet.x, packet.z, min_y, &packet.light_data);
            }
            // A full chunk payload supersedes any individual updates we kept
            // while it was in flight.
            remove_chunk_block_overrides(block_overrides, packet.x, packet.z);
        }
        ClientboundGamePacket::BlockUpdate(packet) => {
            set_block_override(block_overrides, packet.pos, &packet.block_state);
        }
        ClientboundGamePacket::SectionBlocksUpdate(packet) => {
            let mut overrides = block_overrides
                .write()
                .expect("block override lock poisoned");
            for state in &packet.states {
                set_block_override_locked(
                    &mut overrides,
                    packet.section_pos + state.pos,
                    &state.state,
                );
            }
        }
        ClientboundGamePacket::Respawn(_) => {
            block_overrides
                .write()
                .expect("block override lock poisoned")
                .clear();
            store
                .write()
                .expect("lighting cache lock poisoned")
                .clear_lighting();
        }
        ClientboundGamePacket::SetHealth(packet) => {
            hud.write()
                .expect("HUD snapshot lock poisoned")
                .set_health(packet.health, packet.food);
        }
        ClientboundGamePacket::SetExperience(packet) => {
            hud.write()
                .expect("HUD snapshot lock poisoned")
                .set_experience(packet.experience_progress, packet.experience_level);
        }
        ClientboundGamePacket::SetHeldSlot(packet) => {
            hud.write()
                .expect("HUD snapshot lock poisoned")
                .set_selected_hotbar_slot(packet.slot as usize);
        }
        ClientboundGamePacket::ContainerSetContent(packet) if packet.container_id == 0 => {
            let mut hud = hud.write().expect("HUD snapshot lock poisoned");
            for (slot, item) in packet.items.iter().enumerate() {
                update_hud_player_menu_slot(&mut hud, slot, item);
            }
            update_hud_carried_item(&mut hud, &packet.carried_item);
        }
        ClientboundGamePacket::ContainerSetSlot(packet)
            if matches!(packet.container_id, -2 | 0) =>
        {
            update_hud_player_menu_slot(
                &mut hud.write().expect("HUD snapshot lock poisoned"),
                usize::from(packet.slot),
                &packet.item_stack,
            );
        }
        ClientboundGamePacket::SetPlayerInventory(packet) => {
            let mut hud = hud.write().expect("HUD snapshot lock poisoned");
            let slot = packet.slot as usize;
            if slot <= 8 {
                update_hud_hotbar_slot(&mut hud, slot, &packet.contents);
            } else {
                update_hud_player_menu_slot(&mut hud, slot, &packet.contents);
            }
        }
        ClientboundGamePacket::SetCursorItem(packet) => {
            update_hud_carried_item(
                &mut hud.write().expect("HUD snapshot lock poisoned"),
                &packet.contents,
            );
        }
        _ => {}
    }
}

fn update_hud_player_menu_slot(
    hud: &mut HudSnapshot,
    slot: usize,
    item: &azalea::inventory::ItemStack,
) {
    hud.set_player_menu_slot(
        slot,
        item.is_present().then(|| item.kind().to_str()),
        item.count(),
    );
}

fn update_hud_hotbar_slot(hud: &mut HudSnapshot, slot: usize, item: &azalea::inventory::ItemStack) {
    hud.set_hotbar_slot(
        slot,
        item.is_present().then(|| item.kind().to_str()),
        item.count(),
    );
}

fn update_hud_carried_item(hud: &mut HudSnapshot, item: &azalea::inventory::ItemStack) {
    hud.set_carried_item(
        item.is_present().then(|| item.kind().to_str()),
        item.count(),
    );
}

fn set_block_override(
    overrides: &BlockOverrides,
    position: azalea::BlockPos,
    state: &azalea::block::BlockState,
) {
    set_block_override_locked(
        &mut overrides.write().expect("block override lock poisoned"),
        position,
        state,
    );
}

fn set_block_override_locked(
    overrides: &mut HashMap<BlockPos, BlockOverride>,
    position: azalea::BlockPos,
    state: &azalea::block::BlockState,
) {
    let block = block_override_from_id(state.to_trait().id());
    overrides.insert(BlockPos::new(position.x, position.y, position.z), block);
}

fn block_override_from_id(id: &'static str) -> BlockOverride {
    if matches!(id, "air" | "cave_air" | "void_air") {
        BlockOverride::Air
    } else {
        BlockOverride::Solid(id)
    }
}

/// Drop a packet-side override once Azalea's streamed world has applied the
/// exact same rendering-relevant block state. Packet callbacks run before
/// Azalea's world-update system, so keeping it only until this confirmation
/// provides a coherent frame without turning the sidecar into a second,
/// permanent world cache.
fn reconcile_block_overrides(bot: &Client, overrides: &BlockOverrides) {
    let Ok(world) = bot.world() else {
        return;
    };
    let world = world.read();
    let mut overrides = overrides.write().expect("block override lock poisoned");
    retain_unconfirmed_block_overrides(&mut overrides, |position| {
        world
            .get_block_state(azalea::BlockPos::new(position.x, position.y, position.z))
            .map(|state| state.to_trait().id())
    });
}

fn retain_unconfirmed_block_overrides(
    overrides: &mut HashMap<BlockPos, BlockOverride>,
    mut streamed_block_id: impl FnMut(BlockPos) -> Option<&'static str>,
) {
    overrides.retain(|position, expected| {
        streamed_block_id(*position)
            .map(|id| block_override_from_id(id) != *expected)
            .unwrap_or(true)
    });
}

fn remove_chunk_block_overrides(overrides: &BlockOverrides, chunk_x: i32, chunk_z: i32) {
    overrides
        .write()
        .expect("block override lock poisoned")
        .retain(|position, _| {
            position.x.div_euclid(16) != chunk_x || position.z.div_euclid(16) != chunk_z
        });
}

fn drain_player_actions(bot: &Client, hud: &HudSnapshots, receiver: &ActionReceiver) {
    let Ok(receiver) = receiver.lock() else {
        return;
    };
    let actions: Vec<_> = receiver.try_iter().collect();
    drop(receiver);

    for action in actions {
        match action {
            PlayerAction::StartMining(position) => {
                bot.start_mining(azalea::BlockPos::new(position.x, position.y, position.z));
            }
            PlayerAction::UseTargetedBlock(position) => {
                bot.block_interact(azalea::BlockPos::new(position.x, position.y, position.z));
            }
            PlayerAction::SelectHotbarSlot(slot) => {
                bot.set_selected_hotbar_slot(slot);
                hud.write()
                    .expect("HUD snapshot lock poisoned")
                    .set_selected_hotbar_slot(slot as usize);
            }
            PlayerAction::InventoryClick { slot, button } => {
                let Ok(inventory) = bot.get_inventory() else {
                    continue;
                };
                match button {
                    InventoryButton::Left => inventory.left_click(slot),
                    InventoryButton::Right => inventory.right_click(slot),
                    InventoryButton::QuickMove => inventory.shift_click(slot),
                }
            }
        }
    }
}

fn refresh_drowning_indicator(bot: &Client, hud: &HudSnapshots) {
    let Some(ecs) = bot.ecs.try_read() else {
        return;
    };
    let air_supply = ecs.get::<AirSupply>(bot.entity).map(|air| **air);
    let underwater = matches!(
        ecs.get::<FluidOnEyes>(bot.entity),
        Some(fluid) if **fluid == FluidKind::Water
    );
    drop(ecs);

    if air_supply.is_some() {
        hud.write()
            .expect("HUD snapshot lock poisoned")
            .set_air_supply(air_supply, underwater);
    }
}

fn world_min_y(bot: &Client) -> Option<i32> {
    let world = bot.world().ok()?;
    Some(world.read().chunks.min_y())
}

fn apply_light_packet(
    store: &Arc<RwLock<LightStore>>,
    chunk_x: i32,
    chunk_z: i32,
    min_y: i32,
    packet: &azalea::protocol::packets::game::c_light_update::ClientboundLightUpdatePacketData,
) {
    let sky_present: Vec<_> = packet.sky_y_mask.iter_ones().collect();
    let block_present: Vec<_> = packet.block_y_mask.iter_ones().collect();
    let empty_sky: Vec<_> = packet.empty_sky_y_mask.iter_ones().collect();
    let empty_block: Vec<_> = packet.empty_block_y_mask.iter_ones().collect();
    store
        .write()
        .expect("lighting cache lock poisoned")
        .apply_packet(
            chunk_x,
            chunk_z,
            min_y,
            PacketLightData {
                sky_present: &sky_present,
                block_present: &block_present,
                empty_sky: &empty_sky,
                empty_block: &empty_block,
                sky_updates: packet.sky_updates.as_ref(),
                block_updates: packet.block_updates.as_ref(),
            },
        );
}

fn print_live_minimap(
    bot: &Client,
    lighting: &Arc<RwLock<LightStore>>,
    block_overrides: &BlockOverrides,
) -> Result<()> {
    let position = bot.position()?;
    let center = position_to_block(position);
    let world = bot.world()?;
    let world = world.read();
    let lighting = lighting.read().expect("lighting cache lock poisoned");
    let block_overrides = block_overrides
        .read()
        .expect("block override lock poisoned")
        .clone();
    let source = AzaleaWorld::new(&world, &lighting, &block_overrides);
    // ANSI home/clear keeps this diagnostic readable without affecting its
    // direct relationship to live chunk data.
    println!("\x1b[H\x1b[2Jminimap at {center} (Y layer {})", center.y);
    print!("{}", minimap(&source, center, 12));
    Ok(())
}

fn print_forward_hit(
    bot: &Client,
    max_distance: f64,
    lighting: &Arc<RwLock<LightStore>>,
    block_overrides: &BlockOverrides,
) -> Result<()> {
    let eye = bot.eye_position()?;
    let direction = bot.direction()?;
    let camera = Camera {
        origin: Vec3::new(eye.x, eye.y, eye.z),
        yaw_degrees: direction.y_rot(),
        pitch_degrees: direction.x_rot(),
    };
    let (forward, _, _) = camera.basis();
    let world = bot.world()?;
    let world = world.read();
    let lighting = lighting.read().expect("lighting cache lock poisoned");
    let block_overrides = block_overrides
        .read()
        .expect("block override lock poisoned")
        .clone();
    let source = AzaleaWorld::new(&world, &lighting, &block_overrides);
    match raycast(&source, camera.origin, forward, max_distance) {
        RayResult::Hit(hit) => println!(
            "ray: {} at {} ({:.2} blocks, {:?} face)",
            hit.block.id, hit.position, hit.distance, hit.entered_face
        ),
        RayResult::Miss => println!("ray: no block within {max_distance:.0} blocks"),
        RayResult::Unloaded { position, distance } => {
            println!("ray: chunk unloaded at {position} after {distance:.2} blocks")
        }
    }
    Ok(())
}

fn position_to_block(position: azalea::Vec3) -> BlockPos {
    BlockPos::new(
        position.x.floor() as i32,
        position.y.floor() as i32,
        position.z.floor() as i32,
    )
}

fn format_position(position: azalea::Vec3) -> String {
    format!("{:.2} {:.2} {:.2}", position.x, position.y, position.z)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_override_preserves_air_and_solid_states() {
        assert_eq!(block_override_from_id("air"), BlockOverride::Air);
        assert_eq!(block_override_from_id("cave_air"), BlockOverride::Air);
        assert_eq!(block_override_from_id("dirt"), BlockOverride::Solid("dirt"));
    }

    #[test]
    fn confirmed_block_overrides_are_removed_but_unconfirmed_ones_remain() {
        let confirmed = BlockPos::new(1, 64, 1);
        let changed_again = BlockPos::new(2, 64, 1);
        let unloaded = BlockPos::new(3, 64, 1);
        let mut overrides = HashMap::from([
            (confirmed, BlockOverride::Air),
            (changed_again, BlockOverride::Solid("dirt")),
            (unloaded, BlockOverride::Solid("stone")),
        ]);

        retain_unconfirmed_block_overrides(&mut overrides, |position| match position {
            value if value == confirmed => Some("void_air"),
            value if value == changed_again => Some("grass_block"),
            _ => None,
        });

        assert!(!overrides.contains_key(&confirmed));
        assert_eq!(
            overrides.get(&changed_again),
            Some(&BlockOverride::Solid("dirt"))
        );
        assert_eq!(
            overrides.get(&unloaded),
            Some(&BlockOverride::Solid("stone"))
        );
    }

    #[test]
    fn forgetting_a_chunk_removes_only_its_overrides() {
        let overrides: BlockOverrides = Arc::new(RwLock::new(HashMap::from([
            (BlockPos::new(0, 64, 0), BlockOverride::Air),
            (BlockPos::new(-1, 64, -1), BlockOverride::Solid("stone")),
            (BlockPos::new(16, 64, 0), BlockOverride::Solid("dirt")),
        ])));

        remove_chunk_block_overrides(&overrides, -1, -1);
        let overrides = overrides.read().expect("block override lock poisoned");
        assert!(!overrides.contains_key(&BlockPos::new(-1, 64, -1)));
        assert!(overrides.contains_key(&BlockPos::new(0, 64, 0)));
        assert!(overrides.contains_key(&BlockPos::new(16, 64, 0)));
    }
}
