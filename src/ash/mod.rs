//! `/ash` — Active Session History TUI for rpg.
//!
//! # Architecture
//!
//! ```text
//! sampler.rs   — polls pg_stat_activity / ash.samples, aggregates data
//! state.rs     — drill-down state machine, zoom/time-range, key handling
//! renderer.rs  — ratatui widgets: stacked bars, color scheme, layout
//! mod.rs       — public entry point: run_ash()
//! ```
//!
//! # Color scheme (matches pg_ash)
//!
//! | wait_event_type          | Color        |
//! |--------------------------|--------------|
//! | CPU* (uninstrumented)    | Bright green |
//! | Lock                     | Red          |
//! | LWLock                   | Bright yellow|
//! | IO                       | Blue         |
//! | IdleTx (idle in tx)      | Yellow       |
//! | Other                    | Default      |

pub mod renderer;
pub mod sampler;
pub mod state;

use std::time::Duration;

use tokio_postgres::Client;

use crate::config::Settings;

pub use state::AshState;

/// Entry point. Blocks until the user exits with `q` or `Esc`.
pub async fn run_ash(_client: &Client, _settings: &Settings) -> anyhow::Result<()> {
    // TODO: implemented in state.rs / renderer.rs
    let _ = Duration::from_secs(1);
    anyhow::bail!("/ash: not yet implemented")
}
