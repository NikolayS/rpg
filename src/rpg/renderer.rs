#![allow(clippy::all, dead_code, unused_imports)]
/// Terminal output helpers using crossterm.

use crossterm::style::{Attribute, Color, SetAttribute, SetForegroundColor, ResetColor};
use std::io::{self, Write};

pub fn print_room_name(name: &str) {
    let mut out = io::stdout();
    println!();
    crossterm::execute!(out, SetForegroundColor(Color::White), SetAttribute(Attribute::Bold)).ok();
    println!("  {}", name);
    crossterm::execute!(out, ResetColor, SetAttribute(Attribute::Reset)).ok();
}

pub fn print_description(text: &str) {
    println!();
    for line in text.lines() {
        println!("  {}", line);
    }
}

pub fn print_exits(exits: &[crate::rpg::world::Exit]) {
    let mut out = io::stdout();
    print!("\n  Exits: ");
    crossterm::execute!(out, SetForegroundColor(Color::Cyan)).ok();
    let names: Vec<&str> = exits.iter().map(|e| e.direction).collect();
    println!("{}", names.join(", "));
    crossterm::execute!(out, ResetColor).ok();
}

pub fn print_items(items: &[crate::rpg::entities::Item]) {
    if items.is_empty() {
        return;
    }
    let mut out = io::stdout();
    print!("  Items: ");
    crossterm::execute!(out, SetForegroundColor(Color::Yellow)).ok();
    let names: Vec<&str> = items.iter().map(|i| i.name).collect();
    println!("{}", names.join(", "));
    crossterm::execute!(out, ResetColor).ok();
}

pub fn print_enemies(enemies: &[crate::rpg::entities::Enemy]) {
    if enemies.is_empty() {
        return;
    }
    let mut out = io::stdout();
    print!("  Enemies: ");
    crossterm::execute!(out, SetForegroundColor(Color::Red)).ok();
    let names: Vec<&str> = enemies.iter().map(|e| e.name).collect();
    println!("{}", names.join(", "));
    crossterm::execute!(out, ResetColor).ok();
}

pub fn print_info(msg: &str) {
    let mut out = io::stdout();
    crossterm::execute!(out, SetForegroundColor(Color::Cyan)).ok();
    println!("{}", msg);
    crossterm::execute!(out, ResetColor).ok();
}

pub fn print_warn(msg: &str) {
    let mut out = io::stdout();
    crossterm::execute!(out, SetForegroundColor(Color::Yellow)).ok();
    println!("{}", msg);
    crossterm::execute!(out, ResetColor).ok();
}

#[allow(dead_code)]
pub fn print_error(msg: &str) {
    let mut out = io::stdout();
    crossterm::execute!(out, SetForegroundColor(Color::Red)).ok();
    println!("{}", msg);
    crossterm::execute!(out, ResetColor).ok();
}

pub fn print_inventory(items: &[crate::rpg::entities::Item]) {
    println!();
    if items.is_empty() {
        print_warn("  Your inventory is empty.");
        return;
    }
    println!("  Inventory:");
    for item in items {
        let mut out = io::stdout();
        print!("    - ");
        crossterm::execute!(out, SetForegroundColor(Color::Yellow)).ok();
        print!("{}", item.name);
        crossterm::execute!(out, ResetColor).ok();
        println!(" — {}", item.desc);
    }
}

pub fn print_help() {
    println!();
    println!("  Commands:");
    println!("    look / l          — describe the current room");
    println!("    north/n south/s east/e west/w  — move");
    println!("    take <item>       — pick up an item");
    println!("    drop <item>       — drop an item");
    println!("    inventory / i     — list your items");
    println!("    fight <enemy>     — attack an enemy");
    println!("    use <item>        — use an item");
    println!("    examine <target>  — inspect a room, item, or enemy");
    println!("    help / ?          — this message");
    println!("    quit / exit       — leave the game");
}

pub fn print_banner() {
    let mut out = io::stdout();
    println!();
    crossterm::execute!(out, SetForegroundColor(Color::Magenta), SetAttribute(Attribute::Bold)).ok();
    println!("  ╔══════════════════════════════════════════════════╗");
    println!("  ║        THE HAUNTED CLUSTER                       ║");
    println!("  ║        A PostgreSQL Text Adventure               ║");
    println!("  ╚══════════════════════════════════════════════════╝");
    crossterm::execute!(out, ResetColor, SetAttribute(Attribute::Reset)).ok();
    println!();
    println!("  You wake up inside a dying PostgreSQL cluster.");
    println!("  The autovacuum hasn't run in months. Bloat is everywhere.");
    println!("  Find the Absent Daemon and restart it before the cluster falls.");
    println!();
    println!("  Type 'help' for commands. Type 'quit' to escape.");
    println!();
    io::stdout().flush().ok();
}

pub fn print_victory() {
    let mut out = io::stdout();
    println!();
    crossterm::execute!(out, SetForegroundColor(Color::Green), SetAttribute(Attribute::Bold)).ok();
    println!("  ╔══════════════════════════════════════════════════╗");
    println!("  ║                  VICTORY                         ║");
    println!("  ╚══════════════════════════════════════════════════╝");
    crossterm::execute!(out, ResetColor, SetAttribute(Attribute::Reset)).ok();
    println!();
    println!("  The rotting elephant stills. A deep rumble passes through the cluster.");
    println!("  Then: silence.");
    println!();
    println!("  From the dust, a healthy elephant emerges — trunk raised, eyes bright.");
    println!("  It looks at you and nods once.");
    println!();
    println!("  A message appears on every terminal in the cluster:");
    println!();
    crossterm::execute!(out, SetForegroundColor(Color::Cyan)).ok();
    println!("    autovacuum started cleaning up. Finally.");
    crossterm::execute!(out, ResetColor).ok();
    println!();
    println!("  You step out of the cluster. The air smells like fresh WAL.");
    println!();
    io::stdout().flush().ok();
}
