//! Client-owned storage for Minecraft's streamed light and clock data.
//!
//! Azalea deliberately keeps its world representation to blocks and biomes.
//! Minecraft sends lighting separately as packed 4-bit values, so mctui keeps
//! this compact sidecar cache in step with the same chunk packets.

use std::collections::HashMap;

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
}

/// Light data associated with the chunks currently known to mctui.
///
/// A 16×16×16 section needs exactly 2,048 bytes for each kind of light. The
/// maps omit explicitly empty sections instead of retaining zero-filled data.
#[derive(Clone, Debug, Default)]
pub struct LightStore {
    chunks: HashMap<ChunkKey, ChunkLight>,
    time: Option<WorldTime>,
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
        self.time = Some(time);
    }

    pub fn time(&self) -> WorldTime {
        self.time.unwrap_or(WorldTime::NOON)
    }

    pub fn has_received_time(&self) -> bool {
        self.time.is_some()
    }

    pub fn day_factor(&self) -> f32 {
        self.time().day_factor()
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

        clear_sections(&mut chunk.sky, data.empty_sky, min_section);
        clear_sections(&mut chunk.block, data.empty_block, min_section);
        insert_sections(
            &mut chunk.sky,
            data.sky_present,
            data.sky_updates,
            min_section,
        );
        insert_sections(
            &mut chunk.block,
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

        Some(LightLevels::new(
            chunk
                .block
                .get(&section_y)
                .map_or(0, |data| nibble_at(data, index)),
            chunk
                .sky
                .get(&section_y)
                .map_or(0, |data| nibble_at(data, index)),
        ))
    }
}

fn clear_sections(sections: &mut HashMap<i32, Box<[u8]>>, masks: &[usize], min_section: i32) {
    for &mask_index in masks {
        sections.remove(&(min_section + mask_index as i32));
    }
}

fn insert_sections(
    sections: &mut HashMap<i32, Box<[u8]>>,
    masks: &[usize],
    updates: &[Box<[u8]>],
    min_section: i32,
) {
    for (&mask_index, update) in masks.iter().zip(updates) {
        // A malformed packet should not make rendering panic. Valid Minecraft
        // packets always contain exactly 2,048 bytes per selected section.
        if update.len() == LIGHT_SECTION_BYTES {
            sections.insert(min_section + mask_index as i32, update.clone());
        }
    }
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
    fn day_factor_is_bright_at_noon_and_dark_at_midnight() {
        assert!(day_factor_from_ticks(6_000, 0.0) > 0.99);
        assert!(day_factor_from_ticks(18_000, 0.0) < 0.01);
        assert!((0.35..0.65).contains(&day_factor_from_ticks(0, 0.0)));
    }
}
