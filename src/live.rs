//! Azalea integration and the phase-oriented runtime modes.

use std::{
    env, fmt,
    net::IpAddr,
    str::FromStr,
    sync::{
        Arc, RwLock,
        atomic::{AtomicBool, Ordering},
    },
};

use azalea::{
    Client, ClientInformation, Event,
    core::data_registry::DataRegistryWithKey,
    prelude::{Account, Component, bevy_ecs},
    registry::data::WorldClockKey,
};
use eyre::{Result, bail};

use mctui::{
    Block, BlockPos, BlockSource, Camera, LightLevels, RayResult, RenderConfig, Vec3, Voxel,
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
}

impl<'a> AzaleaWorld<'a> {
    pub fn new(world: &'a azalea::world::World, lighting: &'a LightStore) -> Self {
        Self { world, lighting }
    }
}

impl BlockSource for AzaleaWorld<'_> {
    fn voxel_at(&self, position: BlockPos) -> Voxel {
        let position = azalea::BlockPos::new(position.x, position.y, position.z);
        let Some(state) = self.world.get_block_state(position) else {
            return Voxel::Unloaded;
        };
        let id = state.to_trait().id();
        if matches!(id, "air" | "cave_air" | "void_air") {
            Voxel::Air
        } else {
            Voxel::Solid(Block { id })
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

#[derive(Clone, Component)]
pub struct AppState {
    config: Arc<LiveConfig>,
    renderer_started: Arc<AtomicBool>,
    lighting: Arc<RwLock<LightStore>>,
}

impl AppState {
    pub fn new(config: LiveConfig) -> Self {
        Self {
            config: Arc::new(config),
            renderer_started: Arc::new(AtomicBool::new(false)),
            lighting: Arc::new(RwLock::new(LightStore::default())),
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
                std::thread::spawn(move || {
                    if let Err(error) = crate::terminal::run(
                        render_bot.clone(),
                        config.render,
                        config.target_fps,
                        lighting,
                    ) {
                        eprintln!("terminal renderer stopped: {error:?}");
                        render_bot.exit();
                    }
                });
            }
        }
        Event::Packet(packet) => capture_protocol_data(&bot, &state.lighting, &packet),
        Event::Chat(chat) => println!("chat: {}", chat.message().to_ansi()),
        Event::Tick => match state.config.mode {
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
                print_live_minimap(&bot, &state.lighting)?
            }
            Mode::Ray if bot.ticks_connected().is_multiple_of(5) => {
                print_forward_hit(&bot, state.config.render.max_distance, &state.lighting)?
            }
            _ => {}
        },
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
        }
        _ => {}
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

fn print_live_minimap(bot: &Client, lighting: &Arc<RwLock<LightStore>>) -> Result<()> {
    let position = bot.position()?;
    let center = position_to_block(position);
    let world = bot.world()?;
    let world = world.read();
    let lighting = lighting.read().expect("lighting cache lock poisoned");
    let source = AzaleaWorld::new(&world, &lighting);
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
    let source = AzaleaWorld::new(&world, &lighting);
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
