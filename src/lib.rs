//! Rendering primitives for a Minecraft-like voxel world.
//!
//! This module deliberately does not know about Minecraft networking. Keeping
//! ray traversal independent of Azalea makes the important correctness logic
//! cheap to test and lets the live adapter remain a very small boundary.

use std::fmt;

pub mod lighting;

pub use lighting::LightLevels;

/// A three-dimensional point or direction in world units.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vec3 {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

impl Vec3 {
    pub const ZERO: Self = Self::new(0.0, 0.0, 0.0);

    pub const fn new(x: f64, y: f64, z: f64) -> Self {
        Self { x, y, z }
    }

    pub fn length(self) -> f64 {
        self.dot(self).sqrt()
    }

    pub fn normalized(self) -> Option<Self> {
        let length = self.length();
        (length.is_finite() && length > f64::EPSILON).then(|| self / length)
    }

    pub fn dot(self, rhs: Self) -> f64 {
        self.x * rhs.x + self.y * rhs.y + self.z * rhs.z
    }

    pub fn cross(self, rhs: Self) -> Self {
        Self::new(
            self.y * rhs.z - self.z * rhs.y,
            self.z * rhs.x - self.x * rhs.z,
            self.x * rhs.y - self.y * rhs.x,
        )
    }

    pub fn floor_to_block(self) -> BlockPos {
        BlockPos::new(
            self.x.floor() as i32,
            self.y.floor() as i32,
            self.z.floor() as i32,
        )
    }
}

impl std::ops::Add for Vec3 {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        Self::new(self.x + rhs.x, self.y + rhs.y, self.z + rhs.z)
    }
}

impl std::ops::Sub for Vec3 {
    type Output = Self;

    fn sub(self, rhs: Self) -> Self::Output {
        Self::new(self.x - rhs.x, self.y - rhs.y, self.z - rhs.z)
    }
}

impl std::ops::Mul<f64> for Vec3 {
    type Output = Self;

    fn mul(self, rhs: f64) -> Self::Output {
        Self::new(self.x * rhs, self.y * rhs, self.z * rhs)
    }
}

impl std::ops::Div<f64> for Vec3 {
    type Output = Self;

    fn div(self, rhs: f64) -> Self::Output {
        Self::new(self.x / rhs, self.y / rhs, self.z / rhs)
    }
}

/// Integer coordinates of a Minecraft block.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct BlockPos {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

impl BlockPos {
    pub const fn new(x: i32, y: i32, z: i32) -> Self {
        Self { x, y, z }
    }
}

impl fmt::Display for BlockPos {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "({}, {}, {})", self.x, self.y, self.z)
    }
}

/// A minimal, state-independent description of a solid block.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Block {
    /// Minecraft's un-namespaced registry identifier, such as `stone`.
    pub id: &'static str,
}

/// A world sample returned to the raycaster.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Voxel {
    Air,
    Solid(Block),
    /// The chunk is not available locally yet. It must not be treated as air.
    Unloaded,
}

/// Read-only block access needed by the renderer.
pub trait BlockSource: Sync {
    fn voxel_at(&self, position: BlockPos) -> Voxel;

    /// Returns the received light values for a loaded block. Basic synthetic
    /// sources retain a clear daytime default, while the live adapter supplies
    /// the exact packet-backed values.
    fn light_at(&self, _position: BlockPos) -> LightLevels {
        LightLevels::FULL_SKY
    }

    /// Daylight scales sky light without affecting torches or other block
    /// emitters. The default is a fully lit daytime test world.
    fn day_factor(&self) -> f32 {
        1.0
    }
}

/// The face through which a ray entered a block.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Face {
    West,
    East,
    Bottom,
    Top,
    North,
    South,
}

impl Face {
    fn lighting(self) -> f32 {
        match self {
            Self::Top => 1.0,
            Self::Bottom => 0.48,
            Self::North | Self::South => 0.76,
            Self::West | Self::East => 0.84,
        }
    }

    /// The air/fluid cell immediately outside this entered face. Minecraft's
    /// light values describe the space around a solid block, so this is the
    /// value that lights the face a ray can actually see.
    fn light_sample_position(self, position: BlockPos) -> BlockPos {
        match self {
            Self::West => BlockPos::new(position.x - 1, position.y, position.z),
            Self::East => BlockPos::new(position.x + 1, position.y, position.z),
            Self::Bottom => BlockPos::new(position.x, position.y - 1, position.z),
            Self::Top => BlockPos::new(position.x, position.y + 1, position.z),
            Self::North => BlockPos::new(position.x, position.y, position.z - 1),
            Self::South => BlockPos::new(position.x, position.y, position.z + 1),
        }
    }
}

/// A first solid voxel intersected by a ray.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RayHit {
    pub position: BlockPos,
    pub distance: f64,
    pub block: Block,
    /// `None` means the ray started inside the block.
    pub entered_face: Option<Face>,
}

/// The result of traversing the locally loaded voxel grid.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RayResult {
    Hit(RayHit),
    Miss,
    /// Traversal reached an unloaded chunk before a hit or the distance limit.
    Unloaded {
        position: BlockPos,
        distance: f64,
    },
}

/// Traverse a voxel world with Amanatides & Woo DDA.
///
/// `direction` is normalized internally, so distances are always measured in
/// Minecraft blocks even when callers provide a non-unit vector.
pub fn raycast(
    source: &impl BlockSource,
    origin: Vec3,
    direction: Vec3,
    max_distance: f64,
) -> RayResult {
    if !max_distance.is_finite() || max_distance < 0.0 {
        return RayResult::Miss;
    }
    let Some(direction) = direction.normalized() else {
        return RayResult::Miss;
    };

    let mut position = origin.floor_to_block();
    match source.voxel_at(position) {
        Voxel::Solid(block) => {
            return RayResult::Hit(RayHit {
                position,
                distance: 0.0,
                block,
                entered_face: None,
            });
        }
        Voxel::Unloaded => {
            return RayResult::Unloaded {
                position,
                distance: 0.0,
            };
        }
        Voxel::Air => {}
    }

    let (step_x, mut side_x, delta_x) = dda_axis(origin.x, direction.x, position.x);
    let (step_y, mut side_y, delta_y) = dda_axis(origin.y, direction.y, position.y);
    let (step_z, mut side_z, delta_z) = dda_axis(origin.z, direction.z, position.z);
    let mut entered_face;

    loop {
        let distance;
        if side_x <= side_y && side_x <= side_z {
            distance = side_x;
            side_x += delta_x;
            position.x += step_x;
            entered_face = if step_x > 0 { Face::West } else { Face::East };
        } else if side_y <= side_z {
            distance = side_y;
            side_y += delta_y;
            position.y += step_y;
            entered_face = if step_y > 0 { Face::Bottom } else { Face::Top };
        } else {
            distance = side_z;
            side_z += delta_z;
            position.z += step_z;
            entered_face = if step_z > 0 { Face::North } else { Face::South };
        }

        if distance > max_distance {
            return RayResult::Miss;
        }

        match source.voxel_at(position) {
            Voxel::Air => {}
            Voxel::Solid(block) => {
                return RayResult::Hit(RayHit {
                    position,
                    distance,
                    block,
                    entered_face: Some(entered_face),
                });
            }
            Voxel::Unloaded => return RayResult::Unloaded { position, distance },
        }
    }
}

fn dda_axis(origin: f64, direction: f64, block: i32) -> (i32, f64, f64) {
    if direction > 0.0 {
        let delta = 1.0 / direction;
        (1, ((block + 1) as f64 - origin) * delta, delta)
    } else if direction < 0.0 {
        let delta = -1.0 / direction;
        (-1, (origin - block as f64) * delta, delta)
    } else {
        (0, f64::INFINITY, f64::INFINITY)
    }
}

/// An sRGB terminal color.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    pub fn mix(self, other: Self, amount: f32) -> Self {
        let amount = amount.clamp(0.0, 1.0);
        let channel = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * amount).round() as u8;
        Self::new(
            channel(self.r, other.r),
            channel(self.g, other.g),
            channel(self.b, other.b),
        )
    }

    pub fn scale(self, amount: f32) -> Self {
        let amount = amount.clamp(0.0, 1.0);
        Self::new(
            (self.r as f32 * amount).round() as u8,
            (self.g as f32 * amount).round() as u8,
            (self.b as f32 * amount).round() as u8,
        )
    }
}

/// How a block is drawn by the terminal renderer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BlockAppearance {
    pub color: Rgb,
    /// 1 means a ray stops at the block. Lower values are front-to-back
    /// composited with the scene behind it.
    pub opacity: f32,
    /// A full-frame tint used only when the camera is inside this material.
    pub camera_overlay: Option<(Rgb, f32)>,
}

impl BlockAppearance {
    const fn opaque(color: Rgb) -> Self {
        Self {
            color,
            opacity: 1.0,
            camera_overlay: None,
        }
    }

    const fn translucent(color: Rgb, opacity: f32) -> Self {
        Self {
            color,
            opacity,
            camera_overlay: None,
        }
    }

    const fn with_overlay(color: Rgb, opacity: f32, overlay: Rgb, overlay_opacity: f32) -> Self {
        Self {
            color,
            opacity,
            camera_overlay: Some((overlay, overlay_opacity)),
        }
    }

    fn is_translucent(self) -> bool {
        self.opacity < 1.0
    }
}

/// Approximate colors, opacity, and tint for common Minecraft block families.
pub fn block_appearance(id: &str) -> BlockAppearance {
    match id {
        "water" | "bubble_column" => {
            BlockAppearance::with_overlay(Rgb::new(48, 105, 192), 0.36, Rgb::new(25, 78, 150), 0.46)
        }
        "lava" => {
            BlockAppearance::with_overlay(Rgb::new(246, 105, 27), 0.68, Rgb::new(220, 73, 18), 0.58)
        }
        _ if id.contains("glass") => BlockAppearance::translucent(Rgb::new(170, 212, 228), 0.30),
        _ if id.contains("ice") => BlockAppearance::translucent(Rgb::new(144, 195, 220), 0.42),
        "grass_block" | "short_grass" | "fern" | "moss_block" => {
            BlockAppearance::opaque(Rgb::new(94, 159, 53))
        }
        "dirt" | "coarse_dirt" | "rooted_dirt" | "farmland" | "mud" => {
            BlockAppearance::opaque(Rgb::new(126, 92, 57))
        }
        "stone" | "cobblestone" | "andesite" | "diorite" | "granite" => {
            BlockAppearance::opaque(Rgb::new(125, 125, 125))
        }
        "deepslate" | "cobbled_deepslate" | "tuff" => BlockAppearance::opaque(Rgb::new(75, 75, 82)),
        "sand" | "sandstone" | "red_sand" => BlockAppearance::opaque(Rgb::new(215, 198, 138)),
        "snow" | "snow_block" | "powder_snow" | "white_wool" => {
            BlockAppearance::opaque(Rgb::new(233, 239, 245))
        }
        "bedrock" | "obsidian" => BlockAppearance::opaque(Rgb::new(35, 32, 45)),
        "netherrack" | "nether_wart_block" => BlockAppearance::opaque(Rgb::new(118, 45, 43)),
        "end_stone" => BlockAppearance::opaque(Rgb::new(218, 220, 159)),
        // Leaves remain opaque for a legible terminal image, but retain their
        // green tint instead of falling through to neutral stone.
        _ if id.contains("leaves") || id.contains("azalea") => {
            BlockAppearance::opaque(Rgb::new(68, 127, 57))
        }
        _ if id.contains("log") || id.contains("wood") || id.contains("stem") => {
            BlockAppearance::opaque(Rgb::new(103, 78, 48))
        }
        _ if id.contains("planks") || id.contains("slab") || id.contains("stairs") => {
            BlockAppearance::opaque(Rgb::new(174, 137, 83))
        }
        _ if id.contains("ore") => BlockAppearance::opaque(Rgb::new(132, 139, 144)),
        _ if id.contains("terracotta") || id.contains("brick") => {
            BlockAppearance::opaque(Rgb::new(166, 89, 67))
        }
        _ if id.contains("wool") || id.contains("concrete") => {
            BlockAppearance::opaque(Rgb::new(193, 193, 193))
        }
        _ => BlockAppearance::opaque(Rgb::new(151, 151, 151)),
    }
}

/// Kept as a compact palette-only helper for diagnostics and callers that do
/// not need opacity information.
pub fn block_color(id: &str) -> Rgb {
    block_appearance(id).color
}

/// A camera read from the live player entity.
#[derive(Clone, Copy, Debug)]
pub struct Camera {
    pub origin: Vec3,
    /// Minecraft yaw: 0 looks south (+Z), -90 looks east (+X).
    pub yaw_degrees: f32,
    /// Minecraft pitch: negative looks up.
    pub pitch_degrees: f32,
}

/// A read-only entity snapshot used by the terminal renderer.
///
/// Entity collection is deliberately kept outside the raycaster so this crate
/// remains independent of Minecraft networking and ECS implementation details.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EntityMarker {
    /// The entity's feet position in world coordinates.
    pub position: Vec3,
    pub width: f64,
    pub height: f64,
    pub category: EntityCategory,
}

/// Visual group used for a compact entity marker.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntityCategory {
    Player,
    Passive,
    Hostile,
}

impl Camera {
    pub fn basis(self) -> (Vec3, Vec3, Vec3) {
        let yaw = self.yaw_degrees.to_radians() as f64;
        let pitch = self.pitch_degrees.to_radians() as f64;
        let forward = Vec3::new(
            -yaw.sin() * pitch.cos(),
            -pitch.sin(),
            yaw.cos() * pitch.cos(),
        )
        .normalized()
        .expect("a look direction is never zero");
        let right = Vec3::new(yaw.cos(), 0.0, yaw.sin())
            .normalized()
            .expect("a horizontal look direction is never zero");
        let up = forward
            .cross(right)
            .normalized()
            .expect("camera basis vectors are non-parallel");
        (forward, right, up)
    }
}

/// Dimensions and perspective settings for one terminal frame.
#[derive(Clone, Copy, Debug)]
pub struct RenderConfig {
    pub width: usize,
    /// Terminal character rows. Each row represents two ray samples.
    pub height: usize,
    pub horizontal_fov_degrees: f32,
    pub max_distance: f64,
}

impl Default for RenderConfig {
    fn default() -> Self {
        Self {
            width: 80,
            height: 24,
            horizontal_fov_degrees: 75.0,
            max_distance: 48.0,
        }
    }
}

/// A raster at twice the terminal's vertical character resolution.
#[derive(Clone, Debug)]
pub struct Frame {
    pub width: usize,
    pub sample_height: usize,
    pub pixels: Vec<Rgb>,
}

impl Frame {
    pub fn pixel(&self, x: usize, y: usize) -> Rgb {
        self.pixels[y * self.width + x]
    }

    /// Render a frame from the supplied loaded-world view.
    pub fn render(source: &impl BlockSource, camera: Camera, config: RenderConfig) -> Self {
        Self::render_with_entities(source, camera, config, &[])
    }

    /// Render a frame and overlay depth-tested snapshots of nearby entities.
    pub fn render_with_entities(
        source: &impl BlockSource,
        camera: Camera,
        config: RenderConfig,
        entities: &[EntityMarker],
    ) -> Self {
        let width = config.width.max(1);
        let sample_height = config.height.max(1).saturating_mul(2);
        let mut pixels = Vec::with_capacity(width * sample_height);
        let (forward, right, up) = camera.basis();
        let aspect = width as f64 / sample_height as f64;
        let half_fov = (config.horizontal_fov_degrees.to_radians() as f64 / 2.0).tan();

        for y in 0..sample_height {
            let screen_y = 1.0 - 2.0 * (y as f64 + 0.5) / sample_height as f64;
            for x in 0..width {
                let screen_x = 2.0 * (x as f64 + 0.5) / width as f64 - 1.0;
                let direction =
                    (forward + right * (screen_x * aspect * half_fov) + up * (screen_y * half_fov))
                        .normalized()
                        .expect("camera rays are non-zero");
                pixels.push(sample_color(
                    source,
                    camera.origin,
                    direction,
                    config.max_distance,
                ));
            }
        }
        draw_entities(
            source,
            camera,
            config,
            width,
            sample_height,
            &mut pixels,
            entities,
        );
        if let Voxel::Solid(block) = source.voxel_at(camera.origin.floor_to_block())
            && let Some((tint, opacity)) = block_appearance(block.id).camera_overlay
        {
            for pixel in &mut pixels {
                *pixel = pixel.mix(tint, opacity);
            }
        }
        Self {
            width,
            sample_height,
            pixels,
        }
    }
}

fn draw_entities(
    source: &impl BlockSource,
    camera: Camera,
    config: RenderConfig,
    width: usize,
    sample_height: usize,
    pixels: &mut [Rgb],
    entities: &[EntityMarker],
) {
    let (forward, right, up) = camera.basis();
    let aspect = width as f64 / sample_height as f64;
    let half_fov = (config.horizontal_fov_degrees.to_radians() as f64 / 2.0).tan();
    if half_fov <= f64::EPSILON {
        return;
    }

    // Draw far-to-near so an unoccluded closer entity naturally wins when
    // markers overlap on the small terminal raster.
    let mut entities = entities.to_vec();
    entities.sort_by(|left, right_marker| {
        let left_distance = (left.position - camera.origin).length();
        let right_distance = (right_marker.position - camera.origin).length();
        right_distance.total_cmp(&left_distance)
    });

    for entity in entities {
        if entity.width <= 0.0 || entity.height <= 0.0 {
            continue;
        }
        let center = entity.position + Vec3::new(0.0, entity.height * 0.5, 0.0);
        let offset = center - camera.origin;
        let distance = offset.length();
        let forward_distance = offset.dot(forward);
        if !distance.is_finite()
            || distance > config.max_distance
            || forward_distance <= f64::EPSILON
        {
            continue;
        }

        // Trace with the same opaque/translucent semantics as the terrain
        // renderer. Unloaded space is intentionally conservative: a marker
        // must not leak through an unknown chunk.
        match trace_scene(source, camera.origin, offset, distance).terminal {
            RayTerminal::Opaque(hit) if hit.distance < distance - entity.width.max(0.5) * 0.5 => {
                continue;
            }
            RayTerminal::Unloaded { .. } => continue,
            RayTerminal::Opaque(_) | RayTerminal::Miss | RayTerminal::LayerLimit => {}
        }

        let screen_x = offset.dot(right) / (forward_distance * aspect * half_fov);
        let screen_y = offset.dot(up) / (forward_distance * half_fov);
        if !(-1.2..=1.2).contains(&screen_x) || !(-1.2..=1.2).contains(&screen_y) {
            continue;
        }
        let center_x = ((screen_x + 1.0) * 0.5 * width as f64).floor() as isize;
        let center_y = ((1.0 - screen_y) * 0.5 * sample_height as f64).floor() as isize;
        let marker_height =
            ((entity.height / forward_distance / half_fov * sample_height as f64 * 0.5).round()
                as isize)
                .clamp(2, 12);
        let marker_width =
            ((marker_height as f64 * entity.width / entity.height).round() as isize).clamp(1, 6);
        let color = match entity.category {
            EntityCategory::Player => Rgb::new(255, 224, 92),
            EntityCategory::Passive => Rgb::new(104, 226, 167),
            EntityCategory::Hostile => Rgb::new(244, 91, 91),
        };

        let top = center_y - marker_height / 2;
        for y in top..(top + marker_height) {
            let head = y == top;
            let half_width = if head { 0 } else { marker_width / 2 };
            for x in (center_x - half_width)..=(center_x + half_width) {
                if x >= 0 && x < width as isize && y >= 0 && y < sample_height as isize {
                    let pixel = &mut pixels[y as usize * width + x as usize];
                    *pixel = pixel.mix(color, 0.9);
                }
            }
        }
    }
}

fn sample_color(
    source: &impl BlockSource,
    origin: Vec3,
    direction: Vec3,
    max_distance: f64,
) -> Rgb {
    let sky = sky_color(direction.y, source.day_factor());
    let scene = trace_scene(source, origin, direction, max_distance);
    let mut color = match scene.terminal {
        RayTerminal::Opaque(hit) => shade_hit(source, hit, sky, max_distance),
        RayTerminal::Miss | RayTerminal::LayerLimit => sky,
        RayTerminal::Unloaded { distance } => {
            let fog = (distance / max_distance.max(0.001)) as f32;
            Rgb::new(47, 39, 60).mix(sky, fog.clamp(0.0, 1.0) * 0.25)
        }
    };

    // The DDA visits blocks near-to-far. Blend in reverse so the nearest
    // translucent surface is composited last, on top of the full scene behind.
    for hit in scene.layers[..scene.layer_count].iter().rev().flatten() {
        let appearance = block_appearance(hit.block.id);
        color = color.mix(
            shade_hit(source, *hit, sky, max_distance),
            appearance.opacity,
        );
    }
    color
}

const MAX_TRANSLUCENT_LAYERS: usize = 4;

#[derive(Clone, Copy, Debug)]
struct SceneTrace {
    layers: [Option<RayHit>; MAX_TRANSLUCENT_LAYERS],
    layer_count: usize,
    terminal: RayTerminal,
}

#[derive(Clone, Copy, Debug)]
enum RayTerminal {
    Opaque(RayHit),
    Miss,
    Unloaded { distance: f64 },
    LayerLimit,
}

fn trace_scene(
    source: &impl BlockSource,
    origin: Vec3,
    direction: Vec3,
    max_distance: f64,
) -> SceneTrace {
    let mut scene = SceneTrace {
        layers: [None; MAX_TRANSLUCENT_LAYERS],
        layer_count: 0,
        terminal: RayTerminal::Miss,
    };
    if !max_distance.is_finite() || max_distance < 0.0 {
        return scene;
    }
    let Some(direction) = direction.normalized() else {
        return scene;
    };

    let mut position = origin.floor_to_block();
    if let Some(terminal) = trace_voxel(source, position, 0.0, None, &mut scene) {
        scene.terminal = terminal;
        return scene;
    }

    let (step_x, mut side_x, delta_x) = dda_axis(origin.x, direction.x, position.x);
    let (step_y, mut side_y, delta_y) = dda_axis(origin.y, direction.y, position.y);
    let (step_z, mut side_z, delta_z) = dda_axis(origin.z, direction.z, position.z);

    loop {
        let (distance, entered_face) = if side_x <= side_y && side_x <= side_z {
            let distance = side_x;
            side_x += delta_x;
            position.x += step_x;
            (distance, if step_x > 0 { Face::West } else { Face::East })
        } else if side_y <= side_z {
            let distance = side_y;
            side_y += delta_y;
            position.y += step_y;
            (distance, if step_y > 0 { Face::Bottom } else { Face::Top })
        } else {
            let distance = side_z;
            side_z += delta_z;
            position.z += step_z;
            (distance, if step_z > 0 { Face::North } else { Face::South })
        };
        if distance > max_distance {
            return scene;
        }
        if let Some(terminal) =
            trace_voxel(source, position, distance, Some(entered_face), &mut scene)
        {
            scene.terminal = terminal;
            return scene;
        }
    }
}

fn trace_voxel(
    source: &impl BlockSource,
    position: BlockPos,
    distance: f64,
    entered_face: Option<Face>,
    scene: &mut SceneTrace,
) -> Option<RayTerminal> {
    match source.voxel_at(position) {
        Voxel::Air => None,
        Voxel::Unloaded => Some(RayTerminal::Unloaded { distance }),
        Voxel::Solid(block) => {
            let hit = RayHit {
                position,
                distance,
                block,
                entered_face,
            };
            if block_appearance(block.id).is_translucent() {
                if scene.layer_count == MAX_TRANSLUCENT_LAYERS {
                    Some(RayTerminal::LayerLimit)
                } else {
                    scene.layers[scene.layer_count] = Some(hit);
                    scene.layer_count += 1;
                    None
                }
            } else {
                Some(RayTerminal::Opaque(hit))
            }
        }
    }
}

fn shade_hit(source: &impl BlockSource, hit: RayHit, sky: Rgb, max_distance: f64) -> Rgb {
    let face_light = hit.entered_face.map(Face::lighting).unwrap_or(1.0);
    let light_position = hit.entered_face.map_or(hit.position, |face| {
        face.light_sample_position(hit.position)
    });
    let light = source.light_at(light_position);
    let effective = (light.block as f32 / 15.0)
        .max(light.sky as f32 / 15.0 * source.day_factor().clamp(0.0, 1.0));
    // A convex curve gives low light enough lift to be readable, while still
    // making a torch-lit room clearly brighter than an unlit cave.
    let brightness = 0.055 + 0.945 * (1.0 - (1.0 - effective).powf(2.1));
    let fog = ((hit.distance / max_distance.max(0.001)) as f32)
        .clamp(0.0, 1.0)
        .powf(1.65)
        * 0.28;
    block_appearance(hit.block.id)
        .color
        .scale(face_light * brightness)
        .mix(sky, fog)
}

fn sky_color(ray_y: f64, day_factor: f32) -> Rgb {
    let horizon = Rgb::new(132, 189, 233);
    let zenith = Rgb::new(54, 128, 205);
    let ground_haze = Rgb::new(183, 203, 210);
    let daytime = if ray_y >= 0.0 {
        horizon.mix(zenith, (ray_y as f32).clamp(0.0, 1.0).powf(0.55))
    } else {
        horizon.mix(ground_haze, (-ray_y as f32).clamp(0.0, 1.0) * 0.42)
    };
    let night = if ray_y >= 0.0 {
        Rgb::new(8, 16, 36).mix(Rgb::new(20, 35, 70), (ray_y as f32).clamp(0.0, 1.0))
    } else {
        Rgb::new(7, 9, 16)
    };
    daytime.mix(night, 1.0 - day_factor.clamp(0.0, 1.0))
}

/// A small ASCII top-down diagnostic view of a loaded Y layer.
pub fn minimap(source: &impl BlockSource, center: BlockPos, radius: i32) -> String {
    let radius = radius.max(1);
    let mut output = String::new();
    for z in (center.z - radius)..=(center.z + radius) {
        for x in (center.x - radius)..=(center.x + radius) {
            if x == center.x && z == center.z {
                output.push('@');
                continue;
            }
            let character = match source.voxel_at(BlockPos::new(x, center.y, z)) {
                Voxel::Air => ' ',
                Voxel::Unloaded => '?',
                Voxel::Solid(block) => minimap_character(block.id),
            };
            output.push(character);
        }
        output.push('\n');
    }
    output
}

/// A compact navigation map that projects nearby terrain beneath the camera.
///
/// North (-Z) is at the top of the returned map. For each column, the first
/// solid block at or below `center.y` is shown, which makes the map useful
/// while standing above terrain instead of displaying an empty eye-level air
/// slice. `?` still means that no loaded terrain was available in the scan.
pub fn navigation_minimap(
    source: &impl BlockSource,
    center: BlockPos,
    yaw_degrees: f32,
    radius: i32,
) -> String {
    const SURFACE_SCAN_DEPTH: i32 = 8;

    let radius = radius.max(1);
    let mut output = String::new();
    for z in (center.z - radius)..=(center.z + radius) {
        for x in (center.x - radius)..=(center.x + radius) {
            if x == center.x && z == center.z {
                output.push(navigation_heading(yaw_degrees));
            } else {
                output.push(navigation_character(
                    source,
                    BlockPos::new(x, center.y, z),
                    SURFACE_SCAN_DEPTH,
                ));
            }
        }
        output.push('\n');
    }
    output
}

fn navigation_heading(yaw_degrees: f32) -> char {
    // Minecraft yaw 0 faces south (+Z), which is down on the map; -90 faces
    // east (+X), which is to the right.
    let yaw = yaw_degrees.rem_euclid(360.0);
    match yaw {
        value if !(45.0..315.0).contains(&value) => 'v',
        value if value < 135.0 => '<',
        value if value < 225.0 => '^',
        _ => '>',
    }
}

fn navigation_character(source: &impl BlockSource, top: BlockPos, depth: i32) -> char {
    let mut saw_unloaded = false;
    for y in ((top.y - depth)..=top.y).rev() {
        let position = BlockPos::new(top.x, y, top.z);
        match source.voxel_at(position) {
            Voxel::Solid(block) => return minimap_character(block.id),
            Voxel::Unloaded => saw_unloaded = true,
            Voxel::Air => {}
        }
    }
    if saw_unloaded { '?' } else { ' ' }
}

fn minimap_character(id: &str) -> char {
    match id {
        "grass_block" => 'g',
        "dirt" | "coarse_dirt" => 'd',
        "stone" | "cobblestone" | "deepslate" => '#',
        "water" => '~',
        "sand" | "red_sand" => '.',
        _ if id.contains("leaves") || id.contains("log") || id.contains("wood") => 'T',
        _ => '+',
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    #[derive(Default)]
    struct TestWorld(HashMap<BlockPos, Voxel>);

    impl BlockSource for TestWorld {
        fn voxel_at(&self, position: BlockPos) -> Voxel {
            self.0.get(&position).copied().unwrap_or(Voxel::Air)
        }
    }

    struct LightTestWorld {
        blocks: HashMap<BlockPos, Voxel>,
        lights: HashMap<BlockPos, LightLevels>,
    }

    impl BlockSource for LightTestWorld {
        fn voxel_at(&self, position: BlockPos) -> Voxel {
            self.blocks.get(&position).copied().unwrap_or(Voxel::Air)
        }

        fn light_at(&self, position: BlockPos) -> LightLevels {
            self.lights
                .get(&position)
                .copied()
                .unwrap_or(LightLevels::new(0, 0))
        }
    }

    fn stone() -> Voxel {
        Voxel::Solid(Block { id: "stone" })
    }

    #[test]
    fn dda_hits_the_expected_block_and_distance() {
        let mut world = TestWorld::default();
        world.0.insert(BlockPos::new(3, 0, 0), stone());

        let result = raycast(
            &world,
            Vec3::new(0.5, 0.5, 0.5),
            Vec3::new(4.0, 0.0, 0.0),
            10.0,
        );

        assert_eq!(
            result,
            RayResult::Hit(RayHit {
                position: BlockPos::new(3, 0, 0),
                distance: 2.5,
                block: Block { id: "stone" },
                entered_face: Some(Face::West),
            })
        );
    }

    #[test]
    fn dda_does_not_skip_a_diagonal_voxel() {
        let mut world = TestWorld::default();
        world.0.insert(BlockPos::new(1, 0, 1), stone());

        let result = raycast(
            &world,
            Vec3::new(0.2, 0.5, 0.2),
            Vec3::new(1.0, 0.0, 1.0),
            10.0,
        );

        assert!(matches!(
            result,
            RayResult::Hit(RayHit {
                position: BlockPos { x: 1, y: 0, z: 1 },
                ..
            })
        ));
    }

    #[test]
    fn unloaded_space_is_never_rendered_as_sky() {
        let mut world = TestWorld::default();
        world.0.insert(BlockPos::new(1, 0, 0), Voxel::Unloaded);

        let result = raycast(
            &world,
            Vec3::new(0.5, 0.5, 0.5),
            Vec3::new(1.0, 0.0, 0.0),
            10.0,
        );

        assert_eq!(
            result,
            RayResult::Unloaded {
                position: BlockPos::new(1, 0, 0),
                distance: 0.5,
            }
        );
    }

    #[test]
    fn camera_zero_yaw_faces_south() {
        let (forward, right, up) = Camera {
            origin: Vec3::ZERO,
            yaw_degrees: 0.0,
            pitch_degrees: 0.0,
        }
        .basis();
        assert!((forward.z - 1.0).abs() < 1e-10);
        assert!((right.x - 1.0).abs() < 1e-10);
        assert!((up.y - 1.0).abs() < 1e-10);
    }

    #[test]
    fn navigation_minimap_projects_terrain_and_marks_heading() {
        let mut world = TestWorld::default();
        world.0.insert(
            BlockPos::new(-1, 0, -1),
            Voxel::Solid(Block { id: "stone" }),
        );
        world
            .0
            .insert(BlockPos::new(1, 1, 0), Voxel::Solid(Block { id: "water" }));

        let map = navigation_minimap(&world, BlockPos::new(0, 3, 0), 0.0, 1);
        let rows: Vec<_> = map.lines().collect();

        assert_eq!(rows, ["#  ", " v~", "   "]);
        assert_eq!(navigation_heading(-90.0), '>');
        assert_eq!(navigation_heading(90.0), '<');
        assert_eq!(navigation_heading(180.0), '^');
    }

    #[test]
    fn entity_markers_are_hidden_by_terrain() {
        let camera = Camera {
            origin: Vec3::new(0.5, 0.5, 0.5),
            yaw_degrees: 0.0,
            pitch_degrees: 0.0,
        };
        let config = RenderConfig {
            width: 9,
            height: 6,
            horizontal_fov_degrees: 75.0,
            max_distance: 12.0,
        };
        let marker = EntityMarker {
            position: Vec3::new(0.5, 0.0, 5.5),
            width: 0.6,
            height: 1.8,
            category: EntityCategory::Player,
        };
        let open_world = TestWorld::default();
        let empty = Frame::render(&open_world, camera, config);
        let visible = Frame::render_with_entities(&open_world, camera, config, &[marker]);
        assert_ne!(visible.pixels, empty.pixels);

        let mut walled_world = TestWorld::default();
        walled_world.0.insert(BlockPos::new(0, 0, 2), stone());
        let walled = Frame::render(&walled_world, camera, config);
        let hidden = Frame::render_with_entities(&walled_world, camera, config, &[marker]);
        assert_eq!(hidden.pixels, walled.pixels);
    }

    #[test]
    fn translucent_blocks_continue_to_an_opaque_block_behind_them() {
        let mut world = TestWorld::default();
        world
            .0
            .insert(BlockPos::new(1, 0, 0), Voxel::Solid(Block { id: "water" }));
        world.0.insert(BlockPos::new(2, 0, 0), stone());

        let scene = trace_scene(
            &world,
            Vec3::new(0.5, 0.5, 0.5),
            Vec3::new(1.0, 0.0, 0.0),
            10.0,
        );

        assert_eq!(scene.layer_count, 1);
        assert!(matches!(
            scene.terminal,
            RayTerminal::Opaque(RayHit {
                position: BlockPos { x: 2, y: 0, z: 0 },
                ..
            })
        ));
    }

    #[test]
    fn translucent_traversal_caps_at_four_layers() {
        let mut world = TestWorld::default();
        for x in 1..=5 {
            world
                .0
                .insert(BlockPos::new(x, 0, 0), Voxel::Solid(Block { id: "water" }));
        }

        let scene = trace_scene(
            &world,
            Vec3::new(0.5, 0.5, 0.5),
            Vec3::new(1.0, 0.0, 0.0),
            10.0,
        );

        assert_eq!(scene.layer_count, MAX_TRANSLUCENT_LAYERS);
        assert!(matches!(scene.terminal, RayTerminal::LayerLimit));
    }

    #[test]
    fn palette_marks_water_and_glass_translucent_but_leaves_opaque() {
        assert!(block_appearance("water").is_translucent());
        assert!(block_appearance("glass_pane").is_translucent());
        assert!(!block_appearance("oak_leaves").is_translucent());
        assert!(block_appearance("water").camera_overlay.is_some());
        assert!(block_appearance("lava").camera_overlay.is_some());
    }

    #[test]
    fn shading_samples_light_from_the_visible_faces_adjacent_cell() {
        let hit_position = BlockPos::new(1, 0, 0);
        let mut world = LightTestWorld {
            blocks: HashMap::from([(hit_position, stone())]),
            lights: HashMap::from([(BlockPos::new(0, 0, 0), LightLevels::FULL_SKY)]),
        };
        let hit = RayHit {
            position: hit_position,
            distance: 1.0,
            block: Block { id: "stone" },
            entered_face: Some(Face::West),
        };
        let outside_lit = shade_hit(&world, hit, Rgb::new(100, 180, 230), 10.0);

        world.lights.clear();
        let unlit = shade_hit(&world, hit, Rgb::new(100, 180, 230), 10.0);

        assert!(outside_lit.r > unlit.r * 5);
    }
}
