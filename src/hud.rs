//! Packet-backed player HUD state and terminal-friendly formatting.

const HOTBAR_SLOTS: usize = 9;
const PLAYER_MENU_HOTBAR_START: usize = 36;
const BAR_WIDTH: usize = 10;

/// The display data needed for one hotbar cell.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct HotbarSlot {
    item_id: Option<String>,
    count: u32,
}

/// A small, thread-safe-to-clone view of the player's HUD state.
///
/// This intentionally stores only values received from server packets. The
/// terminal renderer can therefore render health and inventory state without
/// touching Azalea's ECS or retaining an inventory lock.
#[derive(Clone, Debug, PartialEq)]
pub struct HudSnapshot {
    health: Option<f32>,
    food: Option<u32>,
    experience_progress: Option<f32>,
    experience_level: Option<u32>,
    selected_hotbar_slot: usize,
    hotbar: [HotbarSlot; HOTBAR_SLOTS],
}

impl Default for HudSnapshot {
    fn default() -> Self {
        Self {
            health: None,
            food: None,
            experience_progress: None,
            experience_level: None,
            selected_hotbar_slot: 0,
            hotbar: std::array::from_fn(|_| HotbarSlot::default()),
        }
    }
}

impl HudSnapshot {
    pub fn set_health(&mut self, health: f32, food: u32) {
        self.health = Some(health.max(0.0));
        self.food = Some(food.min(20));
    }

    pub fn set_experience(&mut self, progress: f32, level: u32) {
        self.experience_progress = Some(progress.clamp(0.0, 1.0));
        self.experience_level = Some(level);
    }

    pub fn set_selected_hotbar_slot(&mut self, slot: usize) {
        if slot < HOTBAR_SLOTS {
            self.selected_hotbar_slot = slot;
        }
    }

    /// Update a player inventory-menu slot if it belongs to the hotbar.
    pub fn set_player_menu_slot(&mut self, slot: usize, item_id: Option<&str>, count: i32) {
        let Some(hotbar_slot) = slot.checked_sub(PLAYER_MENU_HOTBAR_START) else {
            return;
        };
        self.set_hotbar_slot(hotbar_slot, item_id, count);
    }

    /// Update a hotbar-indexed slot, used by the dedicated player-inventory
    /// packet introduced by newer Minecraft protocol versions.
    pub fn set_hotbar_slot(&mut self, slot: usize, item_id: Option<&str>, count: i32) {
        if slot >= HOTBAR_SLOTS {
            return;
        }

        let item_id = item_id
            .filter(|id| *id != "minecraft:air")
            .filter(|_| count > 0)
            .map(|id| id.strip_prefix("minecraft:").unwrap_or(id).to_owned());
        self.hotbar[slot] = HotbarSlot {
            item_id,
            count: count.max(0) as u32,
        };
    }

    pub fn status_line(&self) -> String {
        let health = self
            .health
            .map_or_else(|| "?".to_owned(), format_measurement);
        let food = self
            .food
            .map_or_else(|| "?".to_owned(), |food| food.to_string());
        let level = self
            .experience_level
            .map_or_else(|| "?".to_owned(), |level| level.to_string());

        format!(
            "HP {health:>4}/20 [{}]  Food {food:>2}/20 [{}]  XP Lv {level:>3} [{}]",
            meter(self.health.map(|health| health / 20.0)),
            meter(self.food.map(|food| food as f32 / 20.0)),
            meter(self.experience_progress),
        )
    }

    pub fn hotbar_line(&self) -> String {
        let mut line = String::from("hotbar ");
        for (index, slot) in self.hotbar.iter().enumerate() {
            let cell = if let Some(item_id) = &slot.item_id {
                let code = item_code(item_id);
                let count = slot.count.min(99);
                format!("{}:{}{count:>2}", index + 1, code)
            } else {
                format!("{}:----", index + 1)
            };
            if index == self.selected_hotbar_slot {
                line.push('>');
                line.push_str(&cell);
                line.push('<');
            } else {
                line.push(' ');
                line.push_str(&cell);
                line.push(' ');
            }
        }
        line
    }
}

fn meter(value: Option<f32>) -> String {
    let Some(value) = value else {
        return "?".repeat(BAR_WIDTH);
    };
    let filled = (value.clamp(0.0, 1.0) * BAR_WIDTH as f32).round() as usize;
    format!("{}{}", "#".repeat(filled), "-".repeat(BAR_WIDTH - filled))
}

fn format_measurement(value: f32) -> String {
    if (value.fract()).abs() < 0.05 {
        format!("{value:.0}")
    } else {
        format!("{value:.1}")
    }
}

fn item_code(item_id: &str) -> String {
    let mut words = item_id.split('_').filter(|word| !word.is_empty());
    let first = words.next().unwrap_or("?");
    let second = words.next();
    let mut code = String::new();
    code.push(first.chars().next().unwrap_or('?').to_ascii_uppercase());
    code.push(
        second
            .and_then(|word| word.chars().next())
            .or_else(|| first.chars().nth(1))
            .unwrap_or('?')
            .to_ascii_uppercase(),
    );
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn player_menu_slots_map_only_the_hotbar() {
        let mut hud = HudSnapshot::default();
        hud.set_player_menu_slot(35, Some("minecraft:dirt"), 32);
        hud.set_player_menu_slot(36, Some("minecraft:stone"), 64);
        hud.set_player_menu_slot(44, Some("minecraft:oak_log"), 12);

        assert_eq!(
            hud.hotbar[0],
            HotbarSlot {
                item_id: Some("stone".to_owned()),
                count: 64,
            }
        );
        assert_eq!(
            hud.hotbar[8],
            HotbarSlot {
                item_id: Some("oak_log".to_owned()),
                count: 12,
            }
        );
        assert!(hud.hotbar[1].item_id.is_none());
    }

    #[test]
    fn hud_lines_show_health_food_xp_and_selected_hotbar_slot() {
        let mut hud = HudSnapshot::default();
        hud.set_health(19.5, 17);
        hud.set_experience(0.5, 12);
        hud.set_player_menu_slot(36, Some("minecraft:stone"), 64);
        hud.set_selected_hotbar_slot(0);

        assert_eq!(
            hud.status_line(),
            "HP 19.5/20 [##########]  Food 17/20 [#########-]  XP Lv  12 [#####-----]"
        );
        assert!(hud.hotbar_line().starts_with("hotbar >1:ST64<"));
        assert_eq!(hud.hotbar_line().len(), 79);
    }
}
