/// Turn-based combat engine.

use crate::rpg::entities::{Enemy, ItemKind, Player};
use crate::rpg::renderer::{print_info, print_warn};

pub enum CombatResult {
    Victory,
    PlayerDied,
    Fled,
}

/// Simple LCG RNG (no external deps).
struct SimpleRng(u64);
impl SimpleRng {
    fn new() -> Self {
        // Seed from current time
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(12345);
        Self(seed ^ 0xdeadbeef_cafebabe)
    }
    fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn gen_range(&mut self, min: i32, max: i32) -> i32 {
        let range = (max - min + 1) as u64;
        min + (self.next_u64() % range) as i32
    }
    fn gen_f32(&mut self) -> f32 {
        (self.next_u64() & 0xFFFFFF) as f32 / 0x1000000 as f32
    }
}

pub fn run_combat(player: &mut Player, enemy: &mut Enemy) -> CombatResult {
    let mut rng = SimpleRng::new();
    let mut player_stunned = 0i32;
    let mut vacuum_full_stun = 0i32; // stun after using VACUUM FULL scroll

    println!();
    print_warn(&format!("⚔  Combat begins: {} (HP: {})", enemy.name, enemy.hp));
    println!("   {}", enemy.flavor);
    println!("   Commands: attack (a), use <item>, flee (f)");
    println!();

    loop {
        // Regen from amulet
        if player.has_item(ItemKind::AutovacuumAmulet) && player.hp < player.max_hp {
            player.hp = (player.hp + 5).min(player.max_hp);
        }

        // Show state
        print_hp_bar(player, enemy);
        print!("   > ");
        use std::io::{self, Write};
        io::stdout().flush().ok();

        let mut input = String::new();
        if io::stdin().read_line(&mut input).is_err() {
            return CombatResult::Fled;
        }
        let cmd = input.trim().to_lowercase();

        if player_stunned > 0 {
            player_stunned -= 1;
            print_warn("   You are stunned and cannot act this turn.");
        } else if vacuum_full_stun > 0 {
            vacuum_full_stun -= 1;
            print_warn(&format!("   Stunned by VACUUM FULL side effects ({vacuum_full_stun} turns remaining)."));
        } else if cmd == "a" || cmd == "attack" || cmd.is_empty() {
            let dmg = rng.gen_range(player.attack_min, player.attack_max);
            enemy.hp -= dmg;
            print_info(&format!("   You attack for {} damage.", dmg));
        } else if cmd.starts_with("use ") {
            let item_name = cmd.trim_start_matches("use ").trim();
            let used = use_item_combat(player, enemy, item_name, &mut vacuum_full_stun);
            if !used {
                print_warn("   No such item in your inventory.");
                continue; // don't advance enemy turn
            }
        } else if cmd == "f" || cmd == "flee" {
            let roll = rng.gen_f32();
            if roll < 0.5 {
                print_info("   You flee successfully.");
                return CombatResult::Fled;
            } else {
                print_warn("   You fail to flee!");
            }
        } else {
            print_warn("   Unknown combat command. Try: attack (a), use <item>, flee (f)");
            continue;
        }

        // Check enemy death
        if !enemy.is_alive() {
            println!();
            print_info(&format!("   {} is defeated!", enemy.name));
            victory_flavor(enemy);
            return CombatResult::Victory;
        }

        // Enemy stun check
        if enemy.stunned_turns > 0 {
            enemy.stunned_turns -= 1;
            print_warn(&format!("   {} is stunned and cannot act.", enemy.name));
        } else {
            // N+1 hydra special: gains HP each turn
            if enemy.kind == crate::rpg::entities::EnemyKind::NplusOneHydra {
                enemy.hp += 5;
                enemy.max_hp += 5;
                print_warn("   The N+1 Hydra grows another head. Someone is calling findById() in a loop.");
            }

            let dmg = rng.gen_range(enemy.attack_min, enemy.attack_max);
            player.hp -= dmg;
            print_warn(&format!("   {} attacks you for {} damage.", enemy.name, dmg));

            if enemy.stuns_player {
                let roll = rng.gen_f32();
                if roll < 0.4 {
                    player_stunned = 1;
                    print_warn("   You are stunned!");
                }
            }
        }

        // Check player death
        if !player.is_alive() {
            println!();
            print_warn("   You have died.");
            return CombatResult::PlayerDied;
        }

        // Low HP flavor
        if player.hp <= 10 && player.hp > 0 {
            print_warn("   WARNING: autovacuum is falling behind.");
        }

        println!();
    }
}

fn use_item_combat(
    player: &mut Player,
    enemy: &mut Enemy,
    name: &str,
    vacuum_stun: &mut i32,
) -> bool {
    let mut rng = SimpleRng::new();

    // Try to find item by partial name match
    let kind = find_item_kind(player, name);
    let kind = match kind {
        Some(k) => k,
        None => return false,
    };

    match kind {
        ItemKind::VacuumFullScroll => {
            let dmg = rng.gen_range(40, 60);
            enemy.hp -= dmg;
            *vacuum_stun = 2;
            print_warn(&format!("   VACUUM FULL: {} damage! But you're locked for 2 turns.", dmg));
            player.take_item(ItemKind::VacuumFullScroll);
        }
        ItemKind::ExplainAnalyzeLens => {
            print_info(&format!(
                "   EXPLAIN ANALYZE: {} HP: {}/{}, attack: {}-{}",
                enemy.name, enemy.hp, enemy.max_hp, enemy.attack_min, enemy.attack_max
            ));
            // Don't consume lens
        }
        ItemKind::PgCancelCrossbow => {
            let dmg = rng.gen_range(15, 25);
            enemy.hp -= dmg;
            enemy.stunned_turns = 1;
            print_info(&format!("   pg_cancel_backend: {} damage, enemy stunned next turn.", dmg));
            player.take_item(ItemKind::PgCancelCrossbow);
        }
        ItemKind::ReindexHammer => {
            let dmg = rng.gen_range(20, 35);
            enemy.hp -= dmg;
            print_info(&format!("   REINDEX CONCURRENTLY: {} damage.", dmg));
            player.take_item(ItemKind::ReindexHammer);
        }
        ItemKind::WalSegment => {
            player.hp = (player.hp + 20).min(player.max_hp);
            print_info("   WAL segment: restored 20 HP.");
            player.take_item(ItemKind::WalSegment);
        }
        ItemKind::DiscardAllScroll => {
            print_info("   DISCARD ALL: all debuffs cleared.");
            player.take_item(ItemKind::DiscardAllScroll);
        }
        ItemKind::PgDumpScroll => {
            print_info("   You clutch the pg_dump scroll. It does nothing useful. But somehow you feel better.");
            // Does not consume
        }
        _ => {
            print_warn("   That item has no combat use.");
            return false;
        }
    }
    true
}

fn find_item_kind(player: &Player, name: &str) -> Option<ItemKind> {
    let name_lower = name.to_lowercase();
    player.inventory.iter().find(|i| {
        i.name.to_lowercase().contains(&name_lower)
    }).map(|i| i.kind.clone())
}

fn victory_flavor(enemy: &Enemy) {
    let msg = match enemy.kind {
        crate::rpg::entities::EnemyKind::SeqScanOgre =>
            "   The Seq Scan Ogre collapses. It never knew what an index was.",
        crate::rpg::entities::EnemyKind::LwlockLich =>
            "   The LWLock:LockManager Lich dissolves. Its 847,291 daily summons finally end.",
        crate::rpg::entities::EnemyKind::NplusOneHydra =>
            "   The N+1 Hydra falls. Someone finally used a JOIN.",
        crate::rpg::entities::EnemyKind::AutvacuumBoss =>
            "   The rotting elephant stills. A rumble echoes through the cluster.",
        _ => "   The enemy falls.",
    };
    print_info(msg);
}

fn print_hp_bar(player: &Player, enemy: &Enemy) {
    use crossterm::style::{Color, SetForegroundColor, ResetColor};

    let p_pct = player.hp_pct();
    let p_color = if p_pct > 0.5 { Color::Green } else if p_pct > 0.25 { Color::Yellow } else { Color::Red };
    let e_pct = enemy.hp as f32 / enemy.max_hp as f32;
    let e_color = if e_pct > 0.5 { Color::Green } else if e_pct > 0.25 { Color::Yellow } else { Color::Red };

    let mut out = std::io::stdout();
    print!("   You: ");
    crossterm::execute!(out, SetForegroundColor(p_color)).ok();
    print!("{}/{}", player.hp, player.max_hp);
    crossterm::execute!(out, ResetColor).ok();
    print!("  |  {}: ", enemy.name);
    crossterm::execute!(out, SetForegroundColor(e_color)).ok();
    print!("{}/{}", enemy.hp, enemy.max_hp);
    crossterm::execute!(out, ResetColor).ok();
    println!();
}
