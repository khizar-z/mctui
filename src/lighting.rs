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

    fn advance(self, elapsed_seconds: f64) -> Self {
        // ClockState::rate is expressed in Minecraft ticks per game tick, so
        // a normal rate of 1.0 advances at the 20 Hz game-tick rate.
        let rate = if self.rate.is_finite() {
            self.rate.max(0.0)
        } else {
            0.0
        };
        let elapsed_ticks = f64::from(self.partial_tick.clamp(0.0, 1.0))
            + elapsed_seconds.max(0.0) * 20.0 * f64::from(rate);
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
}

#[derive(Clone, Copy, Debug)]
struct ClockAnchor {
    time: WorldTime,
    received_at: Instant,
}

/// Light data associated with the chunks currently known to mctui.
///
/// A 16×16×16 section needs exactly 2,048 bytes for each kind of light. The
/// maps omit explicitly empty sections instead of retaining zero-filled data.
#[derive(Clone, Debug, Default)]
pub struct LightStore {
    chunks: HashMap<ChunkKey, ChunkLight>,
    time: Option<ClockAnchor>,
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

    pub fn time(&self) -> WorldTime {
        self.time_at(Instant::now())
    }

    pub fn has_received_time(&self) -> bool {
        self.time.is_some()
    }

    pub fn day_factor(&self) -> f32 {
        self.time().day_factor()
    }

    /// Clear streamed light while retaining the latest clock anchor. Respawns
    /// can replace the client world before the next time packet arrives.
    pub fn clear_lighting(&mut self) {
        self.chunks.clear();
    }

    fn set_time_at(&mut self, time: WorldTime, received_at: Instant) {
        self.time = Some(ClockAnchor { time, received_at });
    }

    fn time_at(&self, now: Instant) -> WorldTime {
        self.time.map_or(WorldTime::NOON, |anchor| {
            anchor.time.advance(
                now.saturating_duration_since(anchor.received_at)
                    .as_secs_f64(),
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
            data.empty_sky,
            min_section,
        );
        clear_sections(
            &mut chunk.block,
            &mut chunk.empty_block,
            data.empty_block,
            min_section,
        );
        insert_sections(
            &mut chunk.sky,
            &mut chunk.empty_sky,
            data.sky_present,
            data.sky_updates,
            min_section,
        );
        insert_sections(
            &mut chunk.block,
            &mut chunk.empty_block,
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
        let section_y = position.y.div_euclid(SECTION_EDGE);
        let local_x = position.x.rem_euclid(SECTION_EDGE) as usize;
        let local_y = position.y.rem_euclid(SECTION_EDGE) as usize;
        let local_z = position.z.rem_euclid(SECTION_EDGE) as usize;
        let index = local_x | (local_z << 4) | (local_y << 8);

        // Sparse light packets only describe changed sections. An omitted sky
        // section is unknown, not black: returning None lets the live adapter
        // preserve its explicit bright startup fallback until the server gives
        // us data for this exact section. Known-empty sections still render
        // with zero sky light.
        let sky = light_value(&chunk.sky, &chunk.empty_sky, section_y, index)?;
        let block = light_value(&chunk.block, &chunk.empty_block, section_y, index).unwrap_or(0);
        Some(LightLevels::new(block, sky))
    }
}

fn clear_sections(
    sections: &mut HashMap<i32, Box<[u8]>>,
    empty_sections: &mut HashSet<i32>,
    masks: &[usize],
    min_section: i32,
) {
    for &mask_index in masks {
        let section = min_section + mask_index as i32;
        sections.remove(&section);
        empty_sections.insert(section);
    }
}

fn insert_sections(
    sections: &mut HashMap<i32, Box<[u8]>>,
    empty_sections: &mut HashSet<i32>,
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
        }
    }
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
