//! Packet-backed player HUD state and terminal-friendly formatting.

const HOTBAR_SLOTS: usize = 9;
const PLAYER_MENU_HOTBAR_START: usize = 36;
pub const PLAYER_MENU_SLOTS: usize = 46;
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
    air_supply: Option<i32>,
    underwater: bool,
    selected_hotbar_slot: usize,
    hotbar: [HotbarSlot; HOTBAR_SLOTS],
    player_menu: [HotbarSlot; PLAYER_MENU_SLOTS],
    carried: HotbarSlot,
}

impl Default for HudSnapshot {
    fn default() -> Self {
        Self {
            health: None,
            food: None,
            experience_progress: None,
            experience_level: None,
            air_supply: None,
            underwater: false,
            selected_hotbar_slot: 0,
            hotbar: std::array::from_fn(|_| HotbarSlot::default()),
            player_menu: std::array::from_fn(|_| HotbarSlot::default()),
            carried: HotbarSlot::default(),
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

    /// Update the authoritative air supply and whether the camera is submerged.
    ///
    /// Minecraft sends air supply as entity metadata. The submerged flag comes
    /// from Azalea's fluid-on-eyes calculation, so bubbles only appear while
    /// the player is actually underwater.
    pub fn set_air_supply(&mut self, air_supply: Option<i32>, underwater: bool) {
        self.air_supply = air_supply.map(|air| air.clamp(0, 300));
        self.underwater = underwater;
    }

    pub fn set_selected_hotbar_slot(&mut self, slot: usize) {
        if slot < HOTBAR_SLOTS {
            self.selected_hotbar_slot = slot;
        }
    }

    /// Update a player inventory-menu slot if it belongs to the hotbar.
    pub fn set_player_menu_slot(&mut self, slot: usize, item_id: Option<&str>, count: i32) {
        if slot >= PLAYER_MENU_SLOTS {
            return;
        }

        let item = item_slot(item_id, count);
        self.player_menu[slot] = item.clone();
        if let Some(hotbar_slot) = slot.checked_sub(PLAYER_MENU_HOTBAR_START)
            && hotbar_slot < HOTBAR_SLOTS
        {
            self.hotbar[hotbar_slot] = item;
        }
    }

    /// Update a hotbar-indexed slot, used by the dedicated player-inventory
    /// packet introduced by newer Minecraft protocol versions.
    pub fn set_hotbar_slot(&mut self, slot: usize, item_id: Option<&str>, count: i32) {
        if slot >= HOTBAR_SLOTS {
            return;
        }

        let item = item_slot(item_id, count);
        self.hotbar[slot] = item.clone();
        self.player_menu[PLAYER_MENU_HOTBAR_START + slot] = item;
    }

    pub fn set_carried_item(&mut self, item_id: Option<&str>, count: i32) {
        self.carried = item_slot(item_id, count);
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

        let health_meter = meter(self.health.map(|health| health / 20.0));
        let food_meter = meter(self.food.map(|food| food as f32 / 20.0));
        let experience_meter = meter(self.experience_progress);
        if self.underwater {
            format!(
                "HP {health:>4} [{health_meter}]  Food {food:>2} [{food_meter}]  XP {level:>3} [{experience_meter}] Air [{}]",
                bubble_meter(self.air_supply),
            )
        } else {
            format!(
                "HP {health:>4}/20 [{health_meter}]  Food {food:>2}/20 [{food_meter}]  XP Lv {level:>3} [{experience_meter}]"
            )
        }
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

    /// A fixed-width label for one player-inventory menu slot.
    pub fn inventory_cell(&self, slot: usize, selected: bool) -> String {
        let item = self.player_menu.get(slot).unwrap_or(&self.carried);
        let content = item_cell(item);
        if selected {
            format!(">{content}<")
        } else {
            format!("[{content}]")
        }
    }

    pub fn carried_cell(&self) -> String {
        item_cell(&self.carried)
    }
}

fn item_slot(item_id: Option<&str>, count: i32) -> HotbarSlot {
    let item_id = item_id
        .filter(|id| *id != "minecraft:air")
        .filter(|_| count > 0)
        .map(|id| id.strip_prefix("minecraft:").unwrap_or(id).to_owned());
    HotbarSlot {
        item_id,
        count: count.max(0) as u32,
    }
}

fn meter(value: Option<f32>) -> String {
    let Some(value) = value else {
        return "?".repeat(BAR_WIDTH);
    };
    let filled = (value.clamp(0.0, 1.0) * BAR_WIDTH as f32).round() as usize;
    format!("{}{}", "#".repeat(filled), "-".repeat(BAR_WIDTH - filled))
}

fn bubble_meter(air_supply: Option<i32>) -> String {
    let Some(air_supply) = air_supply else {
        return "?".repeat(BAR_WIDTH);
    };
    let filled = ((air_supply as f32 / 300.0) * BAR_WIDTH as f32).ceil() as usize;
    format!("{}{}", "o".repeat(filled), ".".repeat(BAR_WIDTH - filled))
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

fn item_cell(slot: &HotbarSlot) -> String {
    match &slot.item_id {
        Some(item_id) => format!("{}{:02}", item_code(item_id), slot.count.min(99)),
        None => "----".to_owned(),
    }
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

    #[test]
    fn underwater_status_shows_the_authoritative_air_meter() {
        let mut hud = HudSnapshot::default();
        hud.set_health(20.0, 20);
        hud.set_experience(0.0, 0);
        hud.set_air_supply(Some(150), true);

        let underwater = hud.status_line();
        assert!(underwater.contains("Air [ooooo.....]"));
        assert!(underwater.len() <= 80);
        hud.set_air_supply(Some(300), false);
        assert!(!hud.status_line().contains("Air ["));
    }

    #[test]
    fn inventory_cells_follow_player_menu_updates() {
        let mut hud = HudSnapshot::default();
        hud.set_player_menu_slot(9, Some("minecraft:oak_planks"), 12);
        hud.set_carried_item(Some("minecraft:torch"), 4);

        assert_eq!(hud.inventory_cell(9, true), ">OP12<");
        assert_eq!(hud.carried_cell(), "TO04");
    }
}
