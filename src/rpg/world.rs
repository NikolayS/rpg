#![allow(clippy::all, dead_code, unused_imports)]
/// Room definitions and world map for "The Haunted Cluster".
use crate::rpg::entities::{EnemyKind, Item, ItemKind};

#[derive(Debug, Clone)]
pub struct Exit {
    pub direction: &'static str,
    pub short: &'static str,
    pub to_room: usize,
}

#[derive(Debug, Clone)]
pub struct Puzzle {
    pub prompt: &'static str,
    pub options: &'static [(&'static str, bool, &'static str)], // (text, correct, feedback)
    pub reward: ItemKind,
}

#[derive(Debug, Clone)]
pub struct Room {
    #[allow(dead_code)]
    pub id: usize,
    pub name: &'static str,
    pub description: &'static str,
    pub exits: Vec<Exit>,
    pub items: Vec<Item>,
    pub enemies: Vec<EnemyKind>,
    pub puzzle: Option<Puzzle>,
    pub is_checkpoint: bool,
    pub zone: u8,
    /// Occasional elephant flavor text (shown randomly ~30% of visits)
    pub elephant_event: Option<&'static str>,
}

pub fn build_world() -> Vec<Room> {
    vec![
        // ── Zone 1: The Connection Pool ─────────────────────────────────
        Room {
            id: 0,
            name: "The Lobby",
            description: "Cracked marble floors, flickering fluorescent lights. Hundreds of connection requests queue at a rusted turnstile. An elephant statue in the corner is missing its trunk — someone chiseled it off years ago.\n\nScrawled on the wall: 'NEVER run VACUUM FULL in production without testing. — 2019'",
            exits: vec![
                Exit { direction: "north", short: "n", to_room: 1 },
                Exit { direction: "east",  short: "e", to_room: 2 },
            ],
            items: vec![Item::new(ItemKind::PgDumpScroll)],
            enemies: vec![EnemyKind::ZombieConnection],
            puzzle: None,
            is_checkpoint: true,
            zone: 1,
            elephant_event: Some("A small elephant trots past, muttering something about bloat. It disappears through the wall."),
        },
        Room {
            id: 1,
            name: "pgBouncer Antechamber",
            description: "A narrow waiting room. Connections are pooled here before being allowed through. A sign reads: 'Max pool size: 100. Current: 100. Go away.'\n\nElephant footprints in the dust lead north.",
            exits: vec![
                Exit { direction: "south", short: "s", to_room: 0 },
                Exit { direction: "north", short: "n", to_room: 3 },
            ],
            items: vec![Item::new(ItemKind::DiscardAllScroll)],
            enemies: vec![EnemyKind::ZombieConnection, EnemyKind::ZombieConnection],
            puzzle: Some(Puzzle {
                prompt: "A panicked DBA ghost blocks your path.\n\n'Quick! My app is throwing connection errors. I have max_connections=100. What do I do?'\n\n  a) Increase max_connections to 1000 and restart PostgreSQL\n  b) Deploy PgBouncer in transaction mode\n  c) Kill all idle connections with pg_terminate_backend",
                options: &[
                    ("a) Increase max_connections to 1000", false, "The ghost groans. 'Restart required, and now every connection uses 10MB of RAM. App died anyway.'"),
                    ("b) Deploy PgBouncer in transaction mode", true,  "The ghost relaxes. 'Connection pooling. Of course. You may pass.' It drops a key."),
                    ("c) Kill all idle connections", false, "'They reconnect immediately. I've been doing this for six hours.' The ghost weeps."),
                ],
                reward: ItemKind::ConnectionStringKey,
            }),
            is_checkpoint: false,
            zone: 1,
            elephant_event: None,
        },
        Room {
            id: 2,
            name: "Idle Connection Graveyard",
            description: "Rows of tombstones, each inscribed with a connection string. The epitaphs read:\n  'state: idle, last_active: 14 days ago'\n  'state: idle in transaction, open since 2023'\n  'state: idle, application_name: unknown'\n\nA cold wind carries the smell of TCP keepalives.",
            exits: vec![
                Exit { direction: "west",  short: "w", to_room: 0 },
                Exit { direction: "north", short: "n", to_room: 3 },
            ],
            items: vec![Item::new(ItemKind::WalSegment)],
            enemies: vec![EnemyKind::ZombieConnection, EnemyKind::ZombieConnection, EnemyKind::ZombieConnection],
            puzzle: None,
            is_checkpoint: false,
            zone: 1,
            elephant_event: Some("An elephant skeleton sits at a tombstone. Its plaque reads: 'Sven, connection #847. Idle since boot.'"),
        },
        Room {
            id: 3,
            name: "The Pool Overflow",
            description: "The ceiling drips with rejected connections. A banner reads: 'FATAL: remaining connection slots are reserved for replication.' A massive whirlpool of connection attempts churns in the center.\n\nA door to the south leads back. A dark passage descends east into the Query Catacombs.",
            exits: vec![
                Exit { direction: "south", short: "s", to_room: 1 },
                Exit { direction: "east",  short: "e", to_room: 4 },
            ],
            items: vec![Item::new(ItemKind::PgCancelCrossbow)],
            enemies: vec![EnemyKind::PoolExhaustionWraith],
            puzzle: None,
            is_checkpoint: false,
            zone: 1,
            elephant_event: Some("A young elephant frantically tries to open a door. Each time it gets a FATAL error. It has been here for three days."),
        },

        // ── Zone 2: The Query Catacombs ──────────────────────────────────
        Room {
            id: 4,
            name: "The Slow Query Swamp",
            description: "A fetid bog where queries sink and never return. Query plans float on the surface, all reading 'Seq Scan'. A placard near the entrance notes: 'Average query time: 47 seconds. P99: still running.'\n\nElephant tracks sink deep into the mud heading north.",
            exits: vec![
                Exit { direction: "west",  short: "w", to_room: 3 },
                Exit { direction: "north", short: "n", to_room: 5 },
                Exit { direction: "east",  short: "e", to_room: 6 },
            ],
            items: vec![Item::new(ItemKind::ExplainAnalyzeLens)],
            enemies: vec![EnemyKind::SeqScanOgre],
            puzzle: Some(Puzzle {
                prompt: "A slow query writhes in the mud before you:\n\n  SELECT * FROM orders WHERE created_at > NOW() - interval '7 days'\n\nIt has been running for 4 minutes. A ghost asks: 'How do you fix this?'\n\n  a) Add an index on created_at\n  b) Add an index on id\n  c) VACUUM FULL the orders table",
                options: &[
                    ("a) Add an index on created_at", true,  "The query vanishes in milliseconds. The ghost hands you a lens. 'Always check the WHERE clause first.'"),
                    ("b) Add an index on id",          false, "The query still crawls. 'id is not in the WHERE clause,' the ghost sighs. You lose 5 HP from embarrassment."),
                    ("c) VACUUM FULL orders",           false, "'Table locked for 3 hours. Users very angry. App team called.' You lose 10 HP."),
                ],
                reward: ItemKind::ReindexHammer,
            }),
            is_checkpoint: false,
            zone: 2,
            elephant_event: None,
        },
        Room {
            id: 5,
            name: "N+1 Loop Cavern",
            description: "The walls pulse with identical queries, fired one at a time. A counter on the ceiling reads: SELECT count: 8,247. Table: users. Loop iteration: 8,247.\n\nSomeone, somewhere, is still calling findById() in a loop.",
            exits: vec![
                Exit { direction: "south", short: "s", to_room: 4 },
                Exit { direction: "north", short: "n", to_room: 7 },
            ],
            items: vec![Item::new(ItemKind::StatActivityStone)],
            enemies: vec![EnemyKind::NplusOneHydra],
            puzzle: None,
            is_checkpoint: false,
            zone: 2,
            elephant_event: Some("A small elephant runs past carrying a stack of SELECT statements. It drops them all. They multiply."),
        },
        Room {
            id: 6,
            name: "The Index Graveyard",
            description: "47 unused indexes lie here, monuments to forgotten queries and hasty DBAs. pg_stat_user_indexes shows idx_scan: 0 for all of them.\n\nScratched into a tombstone: 'Here lies idx_users_middle_name. Created in a panic. Never queried. Bloat: 847 MB.'",
            exits: vec![
                Exit { direction: "west",  short: "w", to_room: 4 },
                Exit { direction: "north", short: "n", to_room: 7 },
            ],
            items: vec![Item::new(ItemKind::ReindexHammer)],
            enemies: vec![EnemyKind::SeqScanOgre],
            puzzle: Some(Puzzle {
                prompt: "A DBA spirit hovers over 47 bloated indexes.\n\n'I need to clean these up but I don't know which ones are safe to drop. How do I find unused indexes?'\n\n  a) SELECT indexname FROM pg_stat_user_indexes WHERE idx_scan = 0\n  b) DROP INDEX CONCURRENTLY on all indexes, then recreate the ones that break things\n  c) Run REINDEX DATABASE to refresh all stats",
                options: &[
                    ("a) pg_stat_user_indexes WHERE idx_scan = 0", true,  "The spirit beams. 'Surgical and safe. The stats reset on restart, so filter by pg_stat_reset_time too.' You earn a hammer."),
                    ("b) DROP all, recreate later",                false, "'We lost 6 indexes we actually needed. App is down.' The spirit fades in shame."),
                    ("c) REINDEX DATABASE",                        false, "'That rebuilds indexes, it doesn't identify unused ones.' A 4-hour table lock appears. You lose 15 HP."),
                ],
                reward: ItemKind::AutovacuumAmulet,
            }),
            is_checkpoint: false,
            zone: 2,
            elephant_event: None,
        },
        Room {
            id: 7,
            name: "The Explain Cave",
            description: "Walls covered in EXPLAIN ANALYZE output. Node costs, actual rows, buffers. A particularly large plan covers the entire ceiling:\n\n  -> Seq Scan on orders (cost=0.00..847291.00 rows=42000000)\n       Filter: (status = 'pending')\n       Rows Removed by Filter: 41999837\n\nThe filter selectivity is 0.0000003. Nobody noticed.",
            exits: vec![
                Exit { direction: "south", short: "s", to_room: 5 },
                Exit { direction: "east",  short: "e", to_room: 8 },
            ],
            items: vec![],
            enemies: vec![EnemyKind::DeadlockSpecter],
            puzzle: None,
            is_checkpoint: true,
            zone: 2,
            elephant_event: Some("An elephant reads an EXPLAIN output on the wall, trunk tracing each line carefully. It shakes its head and walks away."),
        },
        Room {
            id: 8,
            name: "Lock Wait Purgatory",
            description: "Dozens of processes hang in the air, suspended mid-query. Each holds a lock the next one wants. pg_locks shows a beautiful cycle: PID 1001 waits for 1002 waits for 1003 waits for 1001.\n\nA throne of deadlocked transactions sits at the center. On it: the LWLock:LockManager Lich.",
            exits: vec![
                Exit { direction: "west",  short: "w", to_room: 7 },
                Exit { direction: "north", short: "n", to_room: 9 },
            ],
            items: vec![Item::new(ItemKind::AutovacuumAmulet)],
            enemies: vec![EnemyKind::LwlockLich],
            puzzle: None,
            is_checkpoint: false,
            zone: 2,
            elephant_event: None,
        },

        // ── Zone 3: The Autovacuum Depths ─────────────────────────────────
        Room {
            id: 9,
            name: "Bloat Cavern",
            description: "Every surface is coated in dead tuples. Tables have grown to 50x their live size. The smell is indescribable.\n\nA sign: 'autovacuum_vacuum_scale_factor = 0.2. Last vacuum: never.'\n\nElephant bones are half-buried in the bloat.",
            exits: vec![
                Exit { direction: "south", short: "s", to_room: 8 },
                Exit { direction: "north", short: "n", to_room: 10 },
                Exit { direction: "east",  short: "e", to_room: 11 },
            ],
            items: vec![Item::new(ItemKind::WalSegment)],
            enemies: vec![EnemyKind::BloatElemental],
            puzzle: None,
            is_checkpoint: true,
            zone: 3,
            elephant_event: Some("A massive elephant wades through the dead tuples. It pauses, looks at you, and says: 'I tried to clean this. autovacuum kept getting cancelled.' It continues wading."),
        },
        Room {
            id: 10,
            name: "Dead Tuple Tomb",
            description: "Millions of dead tuples entombed in amber. Each one is a row that was updated or deleted but never cleaned up. The visibility map is entirely black.\n\nYou can hear autovacuum trying to start, failing, trying again.",
            exits: vec![
                Exit { direction: "south", short: "s", to_room: 9  },
                Exit { direction: "north", short: "n", to_room: 12 },
            ],
            items: vec![Item::new(ItemKind::VacuumFullScroll)],
            enemies: vec![EnemyKind::BloatElemental, EnemyKind::CheckpointThrashHarpy],
            puzzle: None,
            is_checkpoint: false,
            zone: 3,
            elephant_event: None,
        },
        Room {
            id: 11,
            name: "Wraparound Precipice",
            description: "A cliff edge. Below: nothing. The counter on the wall ticks upward:\n\n  age(datfrozenxid): 2,147,483,000\n\nThree more turns and the cluster falls into the void. VACUUM FREEZE pg_class is the only way back.",
            exits: vec![
                Exit { direction: "west",  short: "w", to_room: 9  },
                Exit { direction: "north", short: "n", to_room: 12 },
            ],
            items: vec![],
            enemies: vec![EnemyKind::XidWraparoundDemon],
            puzzle: Some(Puzzle {
                prompt: "The wraparound counter ticks. 3 turns remain.\n\nA ghost screams: 'HOW DO WE STOP IT?'\n\n  a) VACUUM FREEZE pg_class\n  b) VACUUM ANALYZE\n  c) pg_dump the database and restore it",
                options: &[
                    ("a) VACUUM FREEZE pg_class",          true,  "The counter stops. The cluster is saved. The ghost weeps with relief. 'Set vacuum_freeze_min_age lower next time.'"),
                    ("b) VACUUM ANALYZE",                  false, "Not aggressive enough. The counter hits 2,147,483,647. The cluster enters emergency read-only mode. You lose 30 HP."),
                    ("c) pg_dump and restore",              false, "'That takes 6 hours and requires downtime!' The cluster dies. You respawn at the checkpoint."),
                ],
                reward: ItemKind::PgpassCloak,
            }),
            is_checkpoint: false,
            zone: 3,
            elephant_event: None,
        },
        Room {
            id: 12,
            name: "The VACUUM FULL Chamber",
            description: "An operating theater. Tables strapped to the table, being VACUUM FULL'd one by one. Each table is exclusively locked. Nothing can read or write it until the vacuum completes.\n\nA notice: 'Estimated completion: 4 hours. Do not disturb.'\n\nA healthy elephant stands guard, looking nervous.",
            exits: vec![
                Exit { direction: "south", short: "s", to_room: 10 },
                Exit { direction: "east",  short: "e", to_room: 13 },
            ],
            items: vec![Item::new(ItemKind::PgCancelCrossbow)],
            enemies: vec![EnemyKind::CheckpointThrashHarpy],
            puzzle: None,
            is_checkpoint: false,
            zone: 3,
            elephant_event: Some("The elephant guard says: 'I watched a DBA run VACUUM FULL on a 2TB table at 2pm on a Monday. I still have nightmares.'"),
        },
        Room {
            id: 13,
            name: "The Checkpoint",
            description: "A clean room. Bright lights. A single terminal on a desk, blinking steadily. A brass plaque reads:\n\n  'checkpoint_completion_target = 0.9\n   checkpoint_warning = 30s\n   All is well here.'\n\nAn elephant statue made of polished granite watches over the room. Its eyes are kind.",
            exits: vec![
                Exit { direction: "west",  short: "w", to_room: 12 },
                Exit { direction: "north", short: "n", to_room: 14 },
            ],
            items: vec![Item::new(ItemKind::AutovacuumAmulet), Item::new(ItemKind::WalSegment)],
            enemies: vec![],
            puzzle: None,
            is_checkpoint: true,
            zone: 3,
            elephant_event: None,
        },
        Room {
            id: 14,
            name: "pg_upgrade Abyss",
            description: "The final room. The floor is made of pg_catalog tables. Everywhere: dead tuples, unwashed for years. The air smells of old WAL.\n\nAt the center, half-buried in bloat, is a colossal rotting elephant — the Absent Daemon, the autovacuum that stopped running. Its eyes are hollow sockets. Every breath it takes costs the cluster 10ms of I/O.\n\nOn the wall: 'autovacuum: not running'.",
            exits: vec![
                Exit { direction: "south", short: "s", to_room: 13 },
            ],
            items: vec![],
            enemies: vec![EnemyKind::AutvacuumBoss],
            puzzle: None,
            is_checkpoint: false,
            zone: 3,
            elephant_event: None,
        },
    ]
}
