//! Client-owned storage for Minecraft's streamed light and clock data.
//!
//! Azalea deliberately keeps its world representation to blocks and biomes.
//! Minecraft sends lighting separately as packed 4-bit values, so mctui keeps
//! this compact sidecar cache in step with the same chunk packets.

use std::{
    collections::{HashMap, HashSet},
    time::Instant,
};

use crate::BlockPos;

const SECTION_EDGE: i32 = 16;
const LIGHT_SECTION_BYTES: usize = 2_048;
const DEFAULT_SERVER_TICK_RATE: f32 = 20.0;

/// The block and sky light values stored by Minecraft for one block.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LightLevels {
    pub block: u8,
    pub sky: u8,
}

impl LightLevels {
    pub const FULL_SKY: Self = Self { block: 0, sky: 15 };

    pub const fn new(block: u8, sky: u8) -> Self {
        Self { block, sky }
    }
}

/// The server clock used to scale the contribution of sky light.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WorldTime {
    pub total_ticks: u64,
    pub partial_tick: f32,
    pub rate: f32,
}

impl WorldTime {
    pub const NOON: Self = Self {
        total_ticks: 6_000,
        partial_tick: 0.0,
        rate: 1.0,
    };

    pub fn day_factor(self) -> f32 {
        day_factor_from_ticks(self.total_ticks, self.partial_tick)
    }

    fn advance(self, elapsed_seconds: f64, server_tick_rate: f32, server_frozen: bool) -> Self {
        // ClockState::rate is expressed in clock ticks per game tick. The
        // server's game-tick rate is configured independently (for example
        // with `/tick rate`), so both values are needed to advance a clock
        // against wall time.
        let rate = if self.rate.is_finite() {
            self.rate.max(0.0)
        } else {
            0.0
        };
        let server_tick_rate = if server_frozen {
            0.0
        } else if server_tick_rate.is_finite() {
            server_tick_rate.max(0.0)
        } else {
            0.0
        };
        let elapsed_ticks = f64::from(self.partial_tick.clamp(0.0, 1.0))
            + elapsed_seconds.max(0.0) * f64::from(server_tick_rate) * f64::from(rate);
        let whole_ticks = elapsed_ticks.floor().min(u64::MAX as f64) as u64;
        Self {
            total_ticks: self.total_ticks.saturating_add(whole_ticks),
            partial_tick: elapsed_ticks.fract() as f32,
            rate: self.rate,
        }
    }
}

/// Returns a smooth 0..1 daylight multiplier for the Minecraft 24,000-tick
/// day. Noon is 1, midnight is 0, and dawn/dusk are softly blended.
pub fn day_factor_from_ticks(total_ticks: u64, partial_tick: f32) -> f32 {
    let tick = (total_ticks % 24_000) as f32 + partial_tick.clamp(0.0, 1.0);
    // Minecraft's clock has noon at tick 6,000 and midnight at 18,000.
    let sun_height = (tick / 24_000.0 * std::f32::consts::TAU).sin();
    smoothstep(-0.20, 0.20, sun_height)
}

fn smoothstep(edge0: f32, edge1: f32, value: f32) -> f32 {
    let t = ((value - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct ChunkKey {
    x: i32,
    z: i32,
}

#[derive(Clone, Debug, Default)]
struct ChunkLight {
    sky: HashMap<i32, Box<[u8]>>,
    block: HashMap<i32, Box<[u8]>>,
    /// Sections the server explicitly marked as all-zero. This must be kept
    /// separate from absent map entries, which merely mean that a sparse
    /// update did not include that section.
    empty_sky: HashSet<i32>,
    empty_block: HashSet<i32>,
    /// A block-state update can turn a previously dark solid voxel into an
    /// open light sample before its matching light packet arrives. Keep that
    /// one sample unknown instead of incorrectly reusing the old solid's
    /// value.
    invalid_sky: HashSet<(i32, usize)>,
    invalid_block: HashSet<(i32, usize)>,
}

#[derive(Clone, Copy, Debug)]
struct ClockAnchor {
    time: WorldTime,
    received_at: Instant,
}

#[derive(Clone, Copy, Debug)]
struct ServerTickState {
    rate: f32,
    frozen: bool,
}

impl Default for ServerTickState {
    fn default() -> Self {
        Self {
            rate: DEFAULT_SERVER_TICK_RATE,
            frozen: false,
        }
    }
}

/// Light data associated with the chunks currently known to mctui.
///
/// A 16×16×16 section needs exactly 2,048 bytes for each kind of light. The
/// maps omit explicitly empty sections instead of retaining zero-filled data.
#[derive(Clone, Debug, Default)]
pub struct LightStore {
    chunks: HashMap<ChunkKey, ChunkLight>,
    time: Option<ClockAnchor>,
    server_ticks: ServerTickState,
}

/// Borrowed raw light data decoded from one Minecraft light packet.
pub struct PacketLightData<'a> {
    pub sky_present: &'a [usize],
    pub block_present: &'a [usize],
    pub empty_sky: &'a [usize],
    pub empty_block: &'a [usize],
    pub sky_updates: &'a [Box<[u8]>],
    pub block_updates: &'a [Box<[u8]>],
}

impl LightStore {
    pub fn clear(&mut self) {
        self.chunks.clear();
        self.time = None;
        self.server_ticks = ServerTickState::default();
    }

    pub fn remove_chunk(&mut self, chunk_x: i32, chunk_z: i32) {
        self.chunks.remove(&ChunkKey {
            x: chunk_x,
            z: chunk_z,
        });
    }

    pub fn set_time(&mut self, time: WorldTime) {
        self.set_time_at(time, Instant::now());
    }

    /// Update the authoritative game-tick cadence from `TickingState`.
    ///
    /// A world clock's own rate is relative to game ticks; this packet tells
    /// us how many game ticks the server currently runs per wall-clock second.
    pub fn set_server_tick_state(&mut self, rate: f32, frozen: bool) {
        self.set_server_tick_state_at(rate, frozen, Instant::now());
    }

    pub fn time(&self) -> WorldTime {
        self.time_at(Instant::now())
    }

    pub fn has_received_time(&self) -> bool {
        self.time.is_some()
    }

    pub fn day_factor(&self) -> f32 {
        self.time().day_factor()
    }

    pub fn server_tick_rate(&self) -> f32 {
        self.server_ticks.rate
    }

    pub fn is_server_frozen(&self) -> bool {
        self.server_ticks.frozen
    }

    /// Clear streamed light while retaining the latest clock anchor. Respawns
    /// can replace the client world before the next time packet arrives.
    pub fn clear_lighting(&mut self) {
        self.chunks.clear();
    }

    /// Mark the light at a block position stale after a server block update.
    ///
    /// Block and lighting packets are independent. In particular, a block
    /// that just became air must not inherit the old solid block's zero sky
    /// light while waiting for the matching `LightUpdate` packet. The next
    /// update for the affected section clears this marker.
    pub fn invalidate_light_at(&mut self, position: BlockPos) {
        let key = ChunkKey {
            x: position.x.div_euclid(SECTION_EDGE),
            z: position.z.div_euclid(SECTION_EDGE),
        };
        let Some(chunk) = self.chunks.get_mut(&key) else {
            return;
        };
        let (section_y, index) = light_section_and_index(position);
        chunk.invalid_sky.insert((section_y, index));
        chunk.invalid_block.insert((section_y, index));
    }

    fn set_time_at(&mut self, time: WorldTime, received_at: Instant) {
        self.time = Some(ClockAnchor { time, received_at });
    }

    fn set_server_tick_state_at(&mut self, rate: f32, frozen: bool, received_at: Instant) {
        // First advance using the old cadence, then re-anchor at the exact
        // instant the server reports a new cadence. That avoids a visual jump
        // when `/tick rate`, `/tick freeze`, or `/tick unfreeze` is used.
        if let Some(time) = self.time_at_option(received_at) {
            self.time = Some(ClockAnchor { time, received_at });
        }
        self.server_ticks = ServerTickState { rate, frozen };
    }

    fn time_at(&self, now: Instant) -> WorldTime {
        self.time_at_option(now).unwrap_or(WorldTime::NOON)
    }

    fn time_at_option(&self, now: Instant) -> Option<WorldTime> {
        self.time.map(|anchor| {
            anchor.time.advance(
                now.saturating_duration_since(anchor.received_at)
                    .as_secs_f64(),
                self.server_ticks.rate,
                self.server_ticks.frozen,
            )
        })
    }

    /// Apply the four masks and two ordered arrays from a Minecraft light
    /// packet. `min_y` is the world's lowest block Y coordinate.
    ///
    /// Protocol mask bit zero represents the section immediately below the
    /// world's lowest actual section, hence the `min_y / 16 - 1` offset.
    pub fn apply_packet(
        &mut self,
        chunk_x: i32,
        chunk_z: i32,
        min_y: i32,
        data: PacketLightData<'_>,
    ) {
        let min_section = min_y.div_euclid(SECTION_EDGE) - 1;
        let chunk = self
            .chunks
            .entry(ChunkKey {
                x: chunk_x,
                z: chunk_z,
            })
            .or_default();

        clear_sections(
            &mut chunk.sky,
            &mut chunk.empty_sky,
            &mut chunk.invalid_sky,
            data.empty_sky,
            min_section,
        );
        clear_sections(
            &mut chunk.block,
            &mut chunk.empty_block,
            &mut chunk.invalid_block,
            data.empty_block,
            min_section,
        );
        insert_sections(
            &mut chunk.sky,
            &mut chunk.empty_sky,
            &mut chunk.invalid_sky,
            data.sky_present,
            data.sky_updates,
            min_section,
        );
        insert_sections(
            &mut chunk.block,
            &mut chunk.empty_block,
            &mut chunk.invalid_block,
            data.block_present,
            data.block_updates,
            min_section,
        );
    }

    /// Return light data for one world position, if its streamed section is
    /// currently cached.
    pub fn light_at(&self, position: BlockPos) -> Option<LightLevels> {
        let key = ChunkKey {
            x: position.x.div_euclid(SECTION_EDGE),
            z: position.z.div_euclid(SECTION_EDGE),
        };
        let chunk = self.chunks.get(&key)?;
        let (section_y, index) = light_section_and_index(position);

        // Sparse light packets only describe changed sections. An omitted sky
        // section is unknown, not black: returning None lets the live adapter
        // preserve its explicit bright startup fallback until the server gives
        // us data for this exact section. Known-empty sections still render
        // with zero sky light.
        let sky = (!chunk.invalid_sky.contains(&(section_y, index)))
            .then(|| light_value(&chunk.sky, &chunk.empty_sky, section_y, index))
            .flatten()?;
        let block = if chunk.invalid_block.contains(&(section_y, index)) {
            0
        } else {
            light_value(&chunk.block, &chunk.empty_block, section_y, index).unwrap_or(0)
        };
        Some(LightLevels::new(block, sky))
    }
}

fn clear_sections(
    sections: &mut HashMap<i32, Box<[u8]>>,
    empty_sections: &mut HashSet<i32>,
    invalid_samples: &mut HashSet<(i32, usize)>,
    masks: &[usize],
    min_section: i32,
) {
    for &mask_index in masks {
        let section = min_section + mask_index as i32;
        sections.remove(&section);
        empty_sections.insert(section);
        invalid_samples.retain(|(sample_section, _)| *sample_section != section);
    }
}

fn insert_sections(
    sections: &mut HashMap<i32, Box<[u8]>>,
    empty_sections: &mut HashSet<i32>,
    invalid_samples: &mut HashSet<(i32, usize)>,
    masks: &[usize],
    updates: &[Box<[u8]>],
    min_section: i32,
) {
    for (&mask_index, update) in masks.iter().zip(updates) {
        // A malformed packet should not make rendering panic. Valid Minecraft
        // packets always contain exactly 2,048 bytes per selected section.
        if update.len() == LIGHT_SECTION_BYTES {
            let section = min_section + mask_index as i32;
            sections.insert(section, update.clone());
            empty_sections.remove(&section);
            invalid_samples.retain(|(sample_section, _)| *sample_section != section);
        }
    }
}

fn light_section_and_index(position: BlockPos) -> (i32, usize) {
    let section_y = position.y.div_euclid(SECTION_EDGE);
    let local_x = position.x.rem_euclid(SECTION_EDGE) as usize;
    let local_y = position.y.rem_euclid(SECTION_EDGE) as usize;
    let local_z = position.z.rem_euclid(SECTION_EDGE) as usize;
    (section_y, local_x | (local_z << 4) | (local_y << 8))
}

fn light_value(
    sections: &HashMap<i32, Box<[u8]>>,
    empty_sections: &HashSet<i32>,
    section_y: i32,
    index: usize,
) -> Option<u8> {
    sections
        .get(&section_y)
        .map(|data| nibble_at(data, index))
        .or_else(|| empty_sections.contains(&section_y).then_some(0))
}

fn nibble_at(data: &[u8], index: usize) -> u8 {
    let byte = data[index / 2];
    (byte >> ((index & 1) * 4)) & 0x0f
}

#[cfg(test)]
mod tests {
    use super::*;

    fn light_section(index: usize, value: u8) -> Box<[u8]> {
        let mut bytes = vec![0; LIGHT_SECTION_BYTES];
        let shift = (index & 1) * 4;
        bytes[index / 2] = (value & 0x0f) << shift;
        bytes.into_boxed_slice()
    }

    #[test]
    fn packet_masks_map_to_the_correct_negative_world_section() {
        let position = BlockPos::new(2, 3, 4);
        let index = 2 | (4 << 4) | (3 << 8);
        let mut store = LightStore::default();
        // In a -64..320 world, protocol bit 5 is world section 0.
        store.apply_packet(
            0,
            0,
            -64,
            PacketLightData {
                sky_present: &[5],
                block_present: &[5],
                empty_sky: &[],
                empty_block: &[],
                sky_updates: &[light_section(index, 12)],
                block_updates: &[light_section(index, 7)],
            },
        );

        assert_eq!(store.light_at(position), Some(LightLevels::new(7, 12)));
    }

    #[test]
    fn empty_mask_removes_old_light_data() {
        let mut store = LightStore::default();
        store.apply_packet(
            0,
            0,
            0,
            PacketLightData {
                sky_present: &[1],
                block_present: &[],
                empty_sky: &[],
                empty_block: &[],
                sky_updates: &[light_section(0, 15)],
                block_updates: &[],
            },
        );
        store.apply_packet(
            0,
            0,
            0,
            PacketLightData {
                sky_present: &[],
                block_present: &[],
                empty_sky: &[1],
                empty_block: &[],
                sky_updates: &[],
                block_updates: &[],
            },
        );

        assert_eq!(
            store.light_at(BlockPos::new(0, 0, 0)),
            Some(LightLevels::new(0, 0))
        );
    }

    #[test]
    fn sparse_updates_leave_omitted_sky_sections_unknown() {
        let mut store = LightStore::default();
        store.apply_packet(
            0,
            0,
            0,
            PacketLightData {
                sky_present: &[1],
                block_present: &[],
                empty_sky: &[],
                empty_block: &[],
                sky_updates: &[light_section(0, 15)],
                block_updates: &[],
            },
        );

        // Section 1 is known, but section 2 was not declared empty or sent.
        assert_eq!(store.light_at(BlockPos::new(0, 16, 0)), None);
    }

    #[test]
    fn block_change_invalidates_its_old_light_until_a_light_packet_replaces_it() {
        let position = BlockPos::new(2, 3, 4);
        let index = 2 | (4 << 4) | (3 << 8);
        let mut store = LightStore::default();
        store.apply_packet(
            0,
            0,
            0,
            PacketLightData {
                sky_present: &[1],
                block_present: &[1],
                empty_sky: &[],
                empty_block: &[],
                sky_updates: &[light_section(index, 0)],
                block_updates: &[light_section(index, 0)],
            },
        );
        assert_eq!(store.light_at(position), Some(LightLevels::new(0, 0)));

        store.invalidate_light_at(position);
        assert_eq!(store.light_at(position), None);

        store.apply_packet(
            0,
            0,
            0,
            PacketLightData {
                sky_present: &[1],
                block_present: &[1],
                empty_sky: &[],
                empty_block: &[],
                sky_updates: &[light_section(index, 15)],
                block_updates: &[light_section(index, 0)],
            },
        );
        assert_eq!(store.light_at(position), Some(LightLevels::new(0, 15)));
    }

    #[test]
    fn day_factor_is_bright_at_noon_and_dark_at_midnight() {
        assert!(day_factor_from_ticks(6_000, 0.0) > 0.99);
        assert!(day_factor_from_ticks(18_000, 0.0) < 0.01);
        assert!((0.35..0.65).contains(&day_factor_from_ticks(0, 0.0)));
    }

    #[test]
    fn server_clock_advances_smoothly_between_packets() {
        let start = Instant::now();
        let mut store = LightStore::default();
        store.set_time_at(
            WorldTime {
                total_ticks: 6_000,
                partial_tick: 0.25,
                rate: 1.0,
            },
            start,
        );

        let advanced = store.time_at(start + std::time::Duration::from_millis(500));
        assert_eq!(advanced.total_ticks, 6_010);
        assert!((advanced.partial_tick - 0.25).abs() < 0.001);
    }

    #[test]
    fn server_tick_rate_scales_the_clock_against_wall_time() {
        let start = Instant::now();
        let mut store = LightStore::default();
        store.set_server_tick_state_at(200.0, false, start);
        store.set_time_at(
            WorldTime {
                total_ticks: 6_000,
                partial_tick: 0.0,
                rate: 1.0,
            },
            start,
        );

        assert_eq!(
            store
                .time_at(start + std::time::Duration::from_millis(500))
                .total_ticks,
            6_100
        );
    }

    #[test]
    fn changing_or_freezing_the_server_tick_rate_reanchors_without_a_jump() {
        let start = Instant::now();
        let mut store = LightStore::default();
        store.set_time_at(
            WorldTime {
                total_ticks: 6_000,
                partial_tick: 0.0,
                rate: 1.0,
            },
            start,
        );
        let high_rate_at = start + std::time::Duration::from_secs(1);
        store.set_server_tick_state_at(200.0, false, high_rate_at);
        let freeze_at = high_rate_at + std::time::Duration::from_millis(500);
        store.set_server_tick_state_at(200.0, true, freeze_at);

        assert_eq!(store.time_at(freeze_at).total_ticks, 6_120);
        assert_eq!(
            store
                .time_at(freeze_at + std::time::Duration::from_secs(5))
                .total_ticks,
            6_120
        );
    }

    #[test]
    fn stopped_server_clock_does_not_advance() {
        let start = Instant::now();
        let mut store = LightStore::default();
        let time = WorldTime {
            total_ticks: 18_000,
            partial_tick: 0.5,
            rate: 0.0,
        };
        store.set_time_at(time, start);

        assert_eq!(
            store.time_at(start + std::time::Duration::from_secs(10)),
            time
        );
    }
}
