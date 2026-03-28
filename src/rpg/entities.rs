#![allow(clippy::all, dead_code, unused_imports)]
/// Player, Enemy, and Item types for the /rpg MUD.

#[derive(Debug, Clone)]
pub struct Player {
    pub hp: i32,
    pub max_hp: i32,
    pub attack_min: i32,
    pub attack_max: i32,
    pub inventory: Vec<Item>,
    pub checkpoint_room: usize,
}

impl Player {
    pub fn new() -> Self {
        Self {
            hp: 100,
            max_hp: 100,
            attack_min: 10,
            attack_max: 20,
            inventory: Vec::new(),
            checkpoint_room: 0,
        }
    }

    pub fn is_alive(&self) -> bool {
        self.hp > 0
    }

    pub fn hp_pct(&self) -> f32 {
        self.hp as f32 / self.max_hp as f32
    }

    pub fn has_item(&self, kind: ItemKind) -> bool {
        self.inventory.iter().any(|i| i.kind == kind)
    }

    pub fn take_item(&mut self, kind: ItemKind) -> Option<Item> {
        if let Some(pos) = self.inventory.iter().position(|i| i.kind == kind) {
            Some(self.inventory.remove(pos))
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ItemKind {
    VacuumFullScroll,
    ExplainAnalyzeLens,
    PgCancelCrossbow,
    ReindexHammer,
    StatActivityStone,
    AutovacuumAmulet,
    ConnectionStringKey,
    DiscardAllScroll,
    WalSegment,
    PgpassCloak,
    PgDumpScroll,
}

#[derive(Debug, Clone)]
pub struct Item {
    pub kind: ItemKind,
    pub name: &'static str,
    pub desc: &'static str,
}

impl Item {
    pub fn new(kind: ItemKind) -> Self {
        match kind {
            ItemKind::VacuumFullScroll => Item {
                kind,
                name: "VACUUM FULL scroll",
                desc: "Massive damage, but you're stunned for 2 turns (table lock).",
            },
            ItemKind::ExplainAnalyzeLens => Item {
                kind,
                name: "EXPLAIN ANALYZE Lens",
                desc: "Reveals enemy HP and weakness.",
            },
            ItemKind::PgCancelCrossbow => Item {
                kind,
                name: "pg_cancel_backend Crossbow",
                desc: "Ranged attack. Cancels enemy's next turn.",
            },
            ItemKind::ReindexHammer => Item {
                kind,
                name: "REINDEX CONCURRENTLY Hammer",
                desc: "Medium damage. No side effects.",
            },
            ItemKind::StatActivityStone => Item {
                kind,
                name: "pg_stat_activity Stone",
                desc: "See all enemies in the current zone.",
            },
            ItemKind::AutovacuumAmulet => Item {
                kind,
                name: "autovacuum config Amulet",
                desc: "Regenerate 5 HP per turn.",
            },
            ItemKind::ConnectionStringKey => Item {
                kind,
                name: "Connection String Key",
                desc: "Unlocks zone transitions.",
            },
            ItemKind::DiscardAllScroll => Item {
                kind,
                name: "DISCARD ALL scroll",
                desc: "Removes all debuffs.",
            },
            ItemKind::WalSegment => Item {
                kind,
                name: "WAL segment",
                desc: "Restores 20 HP.",
            },
            ItemKind::PgpassCloak => Item {
                kind,
                name: ".pgpass Cloak",
                desc: "Stealth — skip one enemy encounter.",
            },
            ItemKind::PgDumpScroll => Item {
                kind,
                name: "pg_dump scroll",
                desc: "Useless in combat. Gives immense psychological comfort.",
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum EnemyKind {
    ZombieConnection,
    PoolExhaustionWraith,
    SeqScanOgre,
    NplusOneHydra,
    LwlockLich,
    DeadlockSpecter,
    BloatElemental,
    XidWraparoundDemon,
    CheckpointThrashHarpy,
    AutvacuumBoss,
}

#[derive(Debug, Clone)]
pub struct Enemy {
    pub kind: EnemyKind,
    pub name: &'static str,
    pub hp: i32,
    pub max_hp: i32,
    pub attack_min: i32,
    pub attack_max: i32,
    pub flavor: &'static str,
    pub stunned_turns: i32,
    pub stuns_player: bool,
}

impl Enemy {
    pub fn new(kind: EnemyKind) -> Self {
        match kind {
            EnemyKind::ZombieConnection => Enemy {
                kind,
                name: "Zombie Connection",
                hp: 25,
                max_hp: 25,
                attack_min: 3,
                attack_max: 8,
                flavor: "Its last_active was 14 days ago. It has forgotten why it came here.",
                stunned_turns: 0,
                stuns_player: false,
            },
            EnemyKind::PoolExhaustionWraith => Enemy {
                kind,
                name: "Pool Exhaustion Wraith",
                hp: 60,
                max_hp: 60,
                attack_min: 10,
                attack_max: 18,
                flavor: "It whispers: 'remaining: 0/100'. The connection pool is full.",
                stunned_turns: 0,
                stuns_player: false,
            },
            EnemyKind::SeqScanOgre => Enemy {
                kind,
                name: "Seq Scan Ogre",
                hp: 80,
                max_hp: 80,
                attack_min: 12,
                attack_max: 22,
                flavor: "Has been here since 2007. Nobody told him about indexes.",
                stunned_turns: 0,
                stuns_player: false,
            },
            EnemyKind::NplusOneHydra => Enemy {
                kind,
                name: "N+1 Hydra",
                hp: 50,
                max_hp: 50,
                attack_min: 8,
                attack_max: 15,
                flavor: "Every turn it grows another head. Someone is calling findById() in a loop.",
                stunned_turns: 0,
                stuns_player: false,
            },
            EnemyKind::LwlockLich => Enemy {
                kind,
                name: "LWLock:LockManager Lich",
                hp: 120,
                max_hp: 120,
                attack_min: 18,
                attack_max: 30,
                flavor: "'I have been summoned 847,291 times today. I am tired.'",
                stunned_turns: 0,
                stuns_player: true,
            },
            EnemyKind::DeadlockSpecter => Enemy {
                kind,
                name: "Deadlock Specter",
                hp: 40,
                max_hp: 40,
                attack_min: 15,
                attack_max: 25,
                flavor: "ERROR: deadlock detected. DETAIL: Process 12847 waits for ShareLock.",
                stunned_turns: 0,
                stuns_player: true,
            },
            EnemyKind::BloatElemental => Enemy {
                kind,
                name: "Bloat Elemental",
                hp: 70,
                max_hp: 70,
                attack_min: 10,
                attack_max: 20,
                flavor: "It has grown to 40x its original size. Nobody ran VACUUM.",
                stunned_turns: 0,
                stuns_player: false,
            },
            EnemyKind::XidWraparoundDemon => Enemy {
                kind,
                name: "XID Wraparound Demon",
                hp: 90,
                max_hp: 90,
                attack_min: 20,
                attack_max: 35,
                flavor: "age: 2,147,483,000. It counts every transaction. Soon it resets everything.",
                stunned_turns: 0,
                stuns_player: false,
            },
            EnemyKind::CheckpointThrashHarpy => Enemy {
                kind,
                name: "Checkpoint Thrash Harpy",
                hp: 55,
                max_hp: 55,
                attack_min: 8,
                attack_max: 16,
                flavor: "checkpoint_warning: 3.7s. It flaps its wings every 30 seconds.",
                stunned_turns: 0,
                stuns_player: false,
            },
            EnemyKind::AutvacuumBoss => Enemy {
                kind,
                name: "autovacuum: not running",
                hp: 200,
                max_hp: 200,
                attack_min: 25,
                attack_max: 40,
                flavor: "A colossal rotting elephant, half-buried in dead tuples. Its eyes are hollow.",
                stunned_turns: 0,
                stuns_player: false,
            },
        }
    }

    pub fn is_alive(&self) -> bool {
        self.hp > 0
    }
}
