# /rpg — Secret MUD Feature Spec

**Status:** Internal only. Not in any public issue.  
**Feature:** `/rpg` command inside rpg REPL launches a single-player MUD-style text adventure.  
**Integration point:** `src/commands/rpg.rs` — isolated module, easy to drop if we decide not to ship.

---

## Concept

The DB admin types `/rpg` and enters a text adventure set in a dungeon of Postgres horrors. Rooms, items, NPCs, combat, inventory. Classic MUD mechanics but solo. Thematic twist: the dungeon IS a misbehaving PostgreSQL cluster. Monsters are query gremlins, vacuum demons, connection pool zombies. Items are weapons like `VACUUM FULL` (powerful but locks everything), `EXPLAIN ANALYZE` (reveals hidden monster stats), `pg_cancel_backend` (ranged attack).

Exit with `quit`, `exit`, or Ctrl-C.

---

## Architecture

Completely isolated in `src/commands/rpg/`:
```
src/commands/rpg/
  mod.rs          — RpgGame struct, run() entry point
  world.rs        — Room definitions, world map
  entities.rs     — Player, NPC, Item types
  combat.rs       — Turn-based combat engine
  parser.rs       — Command parser (north/n, south/s, look, take, fight, etc.)
  renderer.rs     — Terminal output (colored text, room descriptions)
```

No new external deps. Uses crossterm (already in Cargo.toml) for colored output.

---

## World Design: "The Haunted Cluster"

A dungeon representing a dying Postgres cluster. 3 zones, ~20 rooms total.

### Zone 1 — The Connection Pool (entrance area)
- Rooms: Lobby, pgBouncer Antechamber, Idle Connection Graveyard, The Pool Overflow
- Enemies: Zombie Connections (weak, swarm), Pool Exhaustion Wraith
- Items: `DISCARD ALL` scroll (clears debuffs), Connection String Key
- Lore: Hundreds of idle connections shamble through the corridors. The pool overflows at midnight.

### Zone 2 — The Query Catacombs (mid game)
- Rooms: The Slow Query Swamp, N+1 Loop Cavern, Index Graveyard, The Explain Cave, Lock Wait Purgatory
- Enemies: Seq Scan Ogre (strong, slow), N+1 Hydra (multiplies if not killed fast), LWLock:LockManager Lich (boss), Deadlock Specter (paralyzes)
- Items: `EXPLAIN ANALYZE` Lens (reveals enemy HP/weakness), `CREATE INDEX CONCURRENTLY` Hammer, `pg_cancel_backend` Crossbow (ranged)
- Lore: Queries rot here for eternity. The Seq Scan Ogre has never heard of an index.

### Zone 3 — The Autovacuum Depths (final area)
- Rooms: Bloat Cavern, Dead Tuple Tomb, Wraparound Precipice, The VACUUM FULL Chamber, The Checkpoint (save point), pg_upgrade Abyss (final boss room)
- Enemies: Bloat Elemental, Transaction ID Wraparound Demon (instant kill if you run out of XID), Checkpoint Thrash Harpy
- Final Boss: `autovacuum: not running` — the Absent Daemon, manifested as a colossal rotting elephant
- Victory condition: Restart the autovacuum daemon (find the `pg_ctl reload` spell)

### Items (full list)
| Item | Effect |
|------|--------|
| VACUUM FULL scroll | Massive damage, but you're stunned for 2 turns (table lock) |
| EXPLAIN ANALYZE Lens | Reveals enemy stats and weakness |
| pg_cancel_backend Crossbow | Ranged attack, cancels enemy's next turn |
| REINDEX CONCURRENTLY Hammer | Medium damage, no side effects |
| pg_stat_activity Scrying Stone | See all enemies in current zone |
| autovacuum config Amulet | Regenerate 5 HP per turn |
| Connection String Key | Unlocks zone transitions |
| DISCARD ALL scroll | Removes all debuffs |
| WAL segment (consumable) | +20 HP restore |
| .pgpass Cloak | Stealth — skip one enemy encounter |

---

## Commands

```
look / l              — describe current room
north/n south/s east/e west/w  — move
take <item>           — pick up item
drop <item>           — drop item
inventory / i         — list items
fight / attack <npc>  — start combat
use <item>            — use item
examine <target>      — inspect room/item/npc
help / ?              — show commands
quit / exit           — leave the game (Ctrl-C also works)
```

During combat:
```
attack / a            — basic attack
use <item>            — use item from inventory
flee / f              — attempt to flee (50% chance)
```

---

## Combat System

Turn-based. Player goes first.

- Player: 100 HP, base attack 10-20 damage
- Each item modifies attack or applies effects
- Enemies have HP, attack range, and sometimes special abilities (stun, multiply, drain XP)
- Death: respawn at last checkpoint with 50% HP, lose held items (not inventory weapons)
- Victory: enemy drops loot (random from a pool)

---

## Terminal Rendering

Uses crossterm for:
- Room name: bold white
- Description: normal white  
- Exits: cyan
- Enemies: red
- Items: yellow
- Combat log: alternating white/dim
- HP bar: green→yellow→red based on %

No external TUI library — raw crossterm writes. Keeps it simple and isolated.

---

## Integration into rpg REPL

In `src/commands/mod.rs`, add `/rpg` to the command dispatch table.  
`RpgGame::run()` takes control of the terminal, runs the game loop, then returns cleanly.  
The REPL resumes normally after exit.

---

## Easter Eggs

- If player tries `SELECT * FROM rooms` → "You are not in a SQL environment. Or are you?"
- If player tries `\l` → lists "databases": `template0`, `template1`, `the_void`
- At 1 HP: "WARNING: autovacuum is falling behind"
- Final boss death message: "autovacuum started cleaning up. Finally."

---

## Out of Scope (v1)

- Multiplayer (it's a solo game)
- Persistence/save (session-only, exit loses progress)
- Procedural generation (hand-crafted world only)
- Sound

---

## File Layout (everything stays in src/rpg/)

```
src/rpg/
  mod.rs
  world.rs
  entities.rs
  combat.rs
  parser.rs
  renderer.rs
  demo/
    record-demo.exp     — expect script to record a gameplay session
    slash-rpg-demo.gif  — recorded GIF (committed here, not in demos/)
    slash-rpg-demo.cast — asciinema cast file
```

No AI test scripts. No tests/ai/ involvement. The demo script uses asciinema + expect, same approach as other demos but lives entirely within src/rpg/demo/.

## Demo Recording Script (record-demo.exp)

The expect script should:
1. Launch rpg connected to demo DB: `./target/debug/rpg -U postgres -h demo -p 15433 demo`
2. Type `/rpg` to enter the game
3. Play through Zone 1: look around, pick up an item, fight a zombie connection
4. Encounter one DBA puzzle, solve it correctly
5. Deliver a smart-ass joke moment (visible on screen)
6. Type `quit` to exit the game
7. Total runtime: ~45-60 seconds
8. Prompt: `$ ` (no hostname) via `env PS1="$ " bash --norc --noprofile`

Convert with: `~/.cargo/bin/agg --theme dracula --font-size 13 --line-height 1.1 --speed 1.5 --fps-cap 8`

Target GIF size: under 400K.

---

*Internal spec. Keep off public issues.*
