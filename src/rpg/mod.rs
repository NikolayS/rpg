#![allow(clippy::all, dead_code, unused_imports)]
/// /rpg — The Haunted Cluster: a PostgreSQL text adventure.
///
/// Entirely isolated in src/rpg/. To remove: delete this directory and
/// the `mod rpg;` line in main.rs and the one dispatch arm in
/// src/repl/ai_commands.rs.

mod combat;
mod entities;
mod renderer;
mod world;

use entities::{Enemy, Item, ItemKind, Player};
use renderer::*;
use std::io::{self, BufRead, Write};
use world::{build_world, Room};

pub struct RpgGame {
    rooms: Vec<Room>,
    player: Player,
    current_room: usize,
    /// Items present in each room (mutable, separate from static room def)
    room_items: Vec<Vec<Item>>,
    /// Enemies alive in each room
    room_enemies: Vec<Vec<Enemy>>,
    visit_count: Vec<u32>,
}

impl RpgGame {
    pub fn new() -> Self {
        let rooms = build_world();
        let room_items: Vec<Vec<Item>> = rooms.iter().map(|r| r.items.clone()).collect();
        let room_enemies: Vec<Vec<Enemy>> = rooms
            .iter()
            .map(|r| r.enemies.iter().map(|k| Enemy::new(k.clone())).collect())
            .collect();
        let visit_count = vec![0u32; rooms.len()];
        Self {
            rooms,
            player: Player::new(),
            current_room: 0,
            room_items,
            room_enemies,
            visit_count,
        }
    }

    pub fn run(&mut self) {
        print_banner();
        self.look();
        self.game_loop();
    }

    fn game_loop(&mut self) {
        let stdin = io::stdin();
        loop {
            if self.current_room == usize::MAX {
                break; // victory
            }
            print!("\n  > ");
            io::stdout().flush().ok();
            let mut line = String::new();
            if stdin.lock().read_line(&mut line).is_err() || line.is_empty() {
                break;
            }
            let cmd = line.trim().to_lowercase();
            if cmd.is_empty() {
                continue;
            }
            if !self.handle_command(&cmd) {
                break;
            }
        }
    }

    /// Returns false to quit.
    fn handle_command(&mut self, cmd: &str) -> bool {
        // Easter eggs
        if cmd.starts_with("select") {
            print_info("  You are not in a SQL environment. Or are you?");
            return true;
        }
        if cmd == r"\l" {
            print_info("  databases:");
            print_info("    template0");
            print_info("    template1");
            print_info("    the_void");
            return true;
        }

        match cmd {
            "quit" | "exit" | "q" => {
                print_info("  You step out of the cluster. The bloat remains.");
                return false;
            }
            "look" | "l" => self.look(),
            "help" | "?" | "h" => print_help(),
            "inventory" | "i" | "inv" => print_inventory(&self.player.inventory),
            "north" | "n" => self.go("north"),
            "south" | "s" => self.go("south"),
            "east"  | "e" => self.go("east"),
            "west"  | "w" => self.go("west"),
            _ if cmd.starts_with("take ") => {
                let name = cmd.trim_start_matches("take ").trim();
                self.take_item(name);
            }
            _ if cmd.starts_with("drop ") => {
                let name = cmd.trim_start_matches("drop ").trim();
                self.drop_item(name);
            }
            _ if cmd.starts_with("fight ") || cmd.starts_with("attack ") => {
                let name = cmd.splitn(2, ' ').nth(1).unwrap_or("").trim();
                self.fight(name);
            }
            _ if cmd == "fight" || cmd == "attack" => {
                // fight nearest enemy
                if !self.room_enemies[self.current_room].is_empty() {
                    let name = self.room_enemies[self.current_room][0].name.to_lowercase();
                    self.fight(&name.clone());
                } else {
                    print_warn("  Nothing to fight here.");
                }
            }
            _ if cmd.starts_with("use ") => {
                let name = cmd.trim_start_matches("use ").trim();
                self.use_item(name);
            }
            _ if cmd.starts_with("examine ") => {
                let name = cmd.trim_start_matches("examine ").trim();
                self.examine(name);
            }
            _ => {
                print_warn(&format!("  Unknown command: '{}'. Type 'help' for help.", cmd));
            }
        }
        true
    }

    #[allow(dead_code)]
    fn current(&self) -> &Room {
        &self.rooms[self.current_room]
    }

    fn look(&mut self) {
        let room = &self.rooms[self.current_room];
        print_room_name(room.name);
        print_description(room.description);
        print_exits(&room.exits);
        print_items(&self.room_items[self.current_room]);
        print_enemies(&self.room_enemies[self.current_room]);

        // Checkpoint notice
        if room.is_checkpoint {
            print_info("  [Checkpoint — you will respawn here if you die]");
        }

        // Elephant event (~30% on revisit, always on first visit if set)
        let count = self.visit_count[self.current_room];
        if let Some(event) = room.elephant_event {
            // Simple time-based pseudo-random for elephant events
            let t = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0);
            let show = count == 0 || (t % 10) < 3;
            if show {
                println!();
                print_info(&format!("  {}", event));
            }
        }
        self.visit_count[self.current_room] += 1;
    }

    fn go(&mut self, direction: &str) {
        let exits = self.rooms[self.current_room].exits.clone();
        let target = exits.iter().find(|e| e.direction == direction || e.short == direction);
        match target {
            None => print_warn("  You can't go that way."),
            Some(exit) => {
                let to = exit.to_room;
                // Zone 2/3 locked until player has key
                let from_zone = self.rooms[self.current_room].zone;
                let to_zone   = self.rooms[to].zone;
                if to_zone > from_zone && !self.player.has_item(ItemKind::ConnectionStringKey) {
                    print_warn("  The passage is locked. You need a Connection String Key.");
                    return;
                }
                self.current_room = to;
                self.look();

                // Trigger puzzle if room has one and hasn't been solved
                if self.rooms[self.current_room].puzzle.is_some()
                    && self.visit_count[self.current_room] == 1
                {
                    self.run_puzzle();
                }
            }
        }
    }

    fn take_item(&mut self, name: &str) {
        let items = &mut self.room_items[self.current_room];
        let name_lower = name.to_lowercase();
        let pos = items.iter().position(|i| i.name.to_lowercase().contains(&name_lower));
        match pos {
            None => print_warn("  No such item here."),
            Some(p) => {
                let item = items.remove(p);
                print_info(&format!("  You take the {}.", item.name));
                self.player.inventory.push(item);
            }
        }
    }

    fn drop_item(&mut self, name: &str) {
        let name_lower = name.to_lowercase();
        let pos = self.player.inventory.iter().position(|i| i.name.to_lowercase().contains(&name_lower));
        match pos {
            None => print_warn("  You don't have that item."),
            Some(p) => {
                let item = self.player.inventory.remove(p);
                print_info(&format!("  You drop the {}.", item.name));
                self.room_items[self.current_room].push(item);
            }
        }
    }

    fn fight(&mut self, name: &str) {
        let name_lower = name.to_lowercase();
        let enemies = &mut self.room_enemies[self.current_room];
        let pos = enemies.iter().position(|e| e.name.to_lowercase().contains(&name_lower));
        match pos {
            None => {
                if name.is_empty() || name == "enemy" {
                    print_warn("  No enemies here to fight.");
                } else {
                    print_warn("  No such enemy here.");
                }
            }
            Some(p) => {
                let mut enemy = enemies.remove(p);
                let result = combat::run_combat(&mut self.player, &mut enemy);
                match result {
                    combat::CombatResult::Victory => {
                        // Drop loot
                        let loot = enemy_loot(&enemy.kind);
                        if let Some(item) = loot {
                            print_info(&format!("  {} dropped: {}", enemy.name, item.name));
                            self.room_items[self.current_room].push(item);
                        }
                        // Final boss victory
                        if enemy.kind == entities::EnemyKind::AutvacuumBoss {
                            print_victory();
                            // Signal game over by clearing game state
                            self.current_room = usize::MAX;
                        }
                    }
                    combat::CombatResult::PlayerDied => {
                        // Respawn
                        let checkpoint = find_checkpoint(&self.rooms, self.player.checkpoint_room);
                        self.player.hp = self.player.max_hp / 2;
                        let room_idx = self.current_room;
                        self.current_room = checkpoint;
                        print_warn(&format!("  You respawn at the checkpoint with {} HP.", self.player.hp));
                        // Put enemy back in original room
                        self.room_enemies[room_idx].push(enemy);
                        self.look();
                    }
                    combat::CombatResult::Fled => {
                        // Enemy stays
                        self.room_enemies[self.current_room].push(enemy);
                    }
                }
            }
        }
    }

    fn use_item(&mut self, name: &str) {
        let name_lower = name.to_lowercase();
        let pos = self.player.inventory.iter().position(|i| i.name.to_lowercase().contains(&name_lower));
        match pos {
            None => print_warn("  You don't have that item."),
            Some(p) => {
                let item = &self.player.inventory[p];
                match item.kind {
                    ItemKind::WalSegment => {
                        self.player.hp = (self.player.hp + 20).min(self.player.max_hp);
                        print_info(&format!("  You restore 20 HP. Current HP: {}/{}", self.player.hp, self.player.max_hp));
                        self.player.inventory.remove(p);
                    }
                    ItemKind::DiscardAllScroll => {
                        print_info("  DISCARD ALL. Your debuffs are cleared.");
                        self.player.inventory.remove(p);
                    }
                    ItemKind::PgDumpScroll => {
                        print_info("  You clutch the pg_dump scroll. It does nothing useful. But somehow you feel better.");
                    }
                    ItemKind::StatActivityStone => {
                        println!("  pg_stat_activity for this zone:");
                        for (i, room) in self.rooms.iter().enumerate() {
                            if room.zone == self.rooms[self.current_room].zone {
                                let enemies = &self.room_enemies[i];
                                if !enemies.is_empty() {
                                    let names: Vec<&str> = enemies.iter().map(|e| e.name).collect();
                                    print_info(&format!("    {} — {}", room.name, names.join(", ")));
                                }
                            }
                        }
                    }
                    _ => {
                        print_warn("  Use that item in combat (fight an enemy first).");
                    }
                }
            }
        }
    }

    fn examine(&self, name: &str) {
        let name_lower = name.to_lowercase();
        if name_lower == "room" || name_lower == "here" || name_lower.is_empty() {
            self.look_static();
            return;
        }
        // Check items in room
        if let Some(item) = self.room_items[self.current_room].iter().find(|i| i.name.to_lowercase().contains(&name_lower)) {
            print_info(&format!("  {}: {}", item.name, item.desc));
            return;
        }
        // Check inventory
        if let Some(item) = self.player.inventory.iter().find(|i| i.name.to_lowercase().contains(&name_lower)) {
            print_info(&format!("  {}: {}", item.name, item.desc));
            return;
        }
        // Check enemies
        if let Some(enemy) = self.room_enemies[self.current_room].iter().find(|e| e.name.to_lowercase().contains(&name_lower)) {
            print_warn(&format!("  {}: {}", enemy.name, enemy.flavor));
            return;
        }
        print_warn("  You see nothing notable about that.");
    }

    fn look_static(&self) {
        let room = &self.rooms[self.current_room];
        print_room_name(room.name);
        print_description(room.description);
        print_exits(&room.exits);
        print_items(&self.room_items[self.current_room]);
        print_enemies(&self.room_enemies[self.current_room]);
    }

    fn run_puzzle(&mut self) {
        let puzzle = match &self.rooms[self.current_room].puzzle {
            Some(p) => p.clone(),
            None => return,
        };

        println!();
        print_info("  [PUZZLE]");
        print_description(puzzle.prompt);
        println!();

        let stdin = io::stdin();
        print!("  Your answer (a/b/c): ");
        io::stdout().flush().ok();

        let mut line = String::new();
        if stdin.lock().read_line(&mut line).is_err() {
            return;
        }
        let answer = line.trim().to_lowercase();
        let idx = match answer.as_str() {
            "a" => Some(0usize),
            "b" => Some(1),
            "c" => Some(2),
            _ => None,
        };

        match idx {
            None => print_warn("  Invalid answer. The puzzle remains unsolved."),
            Some(i) => {
                let (_, correct, feedback) = puzzle.options[i];
                if correct {
                    print_info(&format!("  CORRECT! {}", feedback));
                    let reward = Item::new(puzzle.reward.clone());
                    print_info(&format!("  You receive: {}", reward.name));
                    self.player.inventory.push(reward);
                } else {
                    let dmg = 10i32;
                    self.player.hp = (self.player.hp - dmg).max(0);
                    print_warn(&format!("  WRONG! {} You take {} damage.", feedback, dmg));
                }
            }
        }
    }
}

fn find_checkpoint(rooms: &[Room], hint: usize) -> usize {
    // Find nearest checkpoint at or before hint
    for i in (0..=hint.min(rooms.len().saturating_sub(1))).rev() {
        if rooms[i].is_checkpoint {
            return i;
        }
    }
    0
}

fn enemy_loot(kind: &entities::EnemyKind) -> Option<Item> {
    use entities::EnemyKind::*;
    match kind {
        ZombieConnection    => None,
        PoolExhaustionWraith => Some(Item::new(ItemKind::WalSegment)),
        SeqScanOgre         => Some(Item::new(ItemKind::ReindexHammer)),
        NplusOneHydra       => Some(Item::new(ItemKind::ExplainAnalyzeLens)),
        LwlockLich          => Some(Item::new(ItemKind::VacuumFullScroll)),
        DeadlockSpecter     => Some(Item::new(ItemKind::DiscardAllScroll)),
        BloatElemental      => Some(Item::new(ItemKind::WalSegment)),
        XidWraparoundDemon  => Some(Item::new(ItemKind::AutovacuumAmulet)),
        CheckpointThrashHarpy => Some(Item::new(ItemKind::WalSegment)),
        AutvacuumBoss       => None,
    }
}

/// Entry point called from the REPL.
pub fn run_game() {
    let mut game = RpgGame::new();
    // Check if we ended on the victory sentinel
    game.run();
    // game_loop returns when current_room == usize::MAX (victory) or quit
}
