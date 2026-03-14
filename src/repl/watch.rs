//! Watch and observe loop implementation for the REPL.
//!
//! Extracted from `mod.rs` — `watch_query`, `observe_loop`, and helpers.

#![allow(clippy::wildcard_imports)]

use super::ai_commands::{ask_yn_prompt, get_ai_provider, record_token_usage, stream_completion};
use super::*;

/// Default `\watch` interval in seconds.
pub(super) const WATCH_DEFAULT_INTERVAL: f64 = 2.0;

/// Parse an interval string from `\watch [interval]`.
///
/// Accepts:
/// - bare number: `"5"`, `"0.5"` → seconds as f64
/// - `s`-suffixed number: `"5s"`, `"0.5s"` → same
///
/// Returns the default interval (2.0 s) when the string is empty or
/// cannot be parsed as a non-negative number.
pub(super) fn parse_watch_interval(s: &str) -> f64 {
    let s = s.trim();
    if s.is_empty() {
        return WATCH_DEFAULT_INTERVAL;
    }
    // Strip optional trailing `s`.
    let digits = s.strip_suffix('s').unwrap_or(s);
    digits
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite() && *v >= 0.0)
        .unwrap_or(WATCH_DEFAULT_INTERVAL)
}

/// Format a [`std::time::SystemTime`] as psql does for `\watch` headers.
///
/// Produces the ctime-like format that psql uses in local time:
/// `Www Mmm DD HH:MM:SS YYYY`
///
/// Example: `Thu Mar 13 19:00:00 2026`
///
/// On Unix, uses `libc::localtime_r` to convert to local time so the
/// output matches the user's timezone, consistent with psql behaviour.
/// On Windows, falls back to UTC (local-time conversion via POSIX
/// `localtime_r` is not available on that platform).
pub(super) fn format_system_time(now: std::time::SystemTime) -> String {
    const WDAY: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MON: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];

    use std::time::{Duration, UNIX_EPOCH};

    let duration = now.duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO);

    // Windows path: pure-Rust UTC computation.
    // localtime_r is POSIX-only and unavailable on Windows; UTC is an
    // acceptable fallback for the \watch timestamp display on Windows.
    #[cfg(windows)]
    return {
        let secs = duration.as_secs();

        // Decompose unix timestamp into calendar fields (UTC).
        // Algorithm: http://howardhinnant.github.io/date_algorithms.html
        let days = secs / 86_400;
        let time_of_day = secs % 86_400;
        let hour = (time_of_day / 3_600) as u32;
        let min = ((time_of_day % 3_600) / 60) as u32;
        let sec = (time_of_day % 60) as u32;

        // days since Unix epoch → civil date (Gregorian proleptic)
        let z = days as i64 + 719_468;
        let era: i64 = if z >= 0 { z } else { z - 146_096 } / 146_097;
        let doe = (z - era * 146_097) as u64;
        let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
        let y = yoe as i64 + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        let y = if m <= 2 { y + 1 } else { y };

        // Day-of-week: 0=Sun (Unix epoch was a Thursday = 4)
        let wday_idx = ((days + 4) % 7) as usize;
        let mon = MON[(m - 1).clamp(0, 11) as usize];
        let wday = WDAY[wday_idx.clamp(0, 6)];

        format!("{wday} {mon} {d:2} {hour:02}:{min:02}:{sec:02} {y}")
    };

    // Unix path: use libc::localtime_r for local-time conversion.
    // `#[allow(deprecated)]` silences the musl time_t 32→64-bit transition
    // warning; the cast is correct on all 64-bit targets Rpg ships for.
    #[cfg(not(windows))]
    {
        #[allow(deprecated)]
        #[allow(clippy::cast_possible_wrap)]
        let unix_secs: libc::time_t = duration.as_secs() as libc::time_t;

        // SAFETY: localtime_r is thread-safe and only reads the time_t we pass.
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        unsafe {
            libc::localtime_r(&raw const unix_secs, &raw mut tm);
        }

        // clamp() guarantees the index is in-bounds; the value was already
        // non-negative so the cast to usize is safe.
        #[allow(clippy::cast_sign_loss)]
        let wday = WDAY[tm.tm_wday.clamp(0, 6) as usize];
        #[allow(clippy::cast_sign_loss)]
        let mon = MON[tm.tm_mon.clamp(0, 11) as usize];
        let day = tm.tm_mday;
        let hour = tm.tm_hour;
        let min = tm.tm_min;
        let sec = tm.tm_sec;
        let year = tm.tm_year + 1900;

        format!("{wday} {mon} {day:2} {hour:02}:{min:02}:{sec:02} {year}")
    }
}

/// Re-execute `sql` repeatedly, printing a timestamp header before each run.
///
/// The loop exits when Ctrl-C (SIGINT) is received while sleeping between
/// iterations.  Each iteration:
/// 1. Prints timestamp header matching psql's ctime-like format.
/// 2. Executes the query.
/// 3. Sleeps `interval_secs`; if Ctrl-C arrives during the sleep, exits.
pub(super) async fn watch_query(
    client: &Client,
    sql: &str,
    interval_secs: f64,
    settings: &mut ReplSettings,
) {
    use std::time::Duration;
    use tokio::signal;
    use tokio::time::sleep;

    loop {
        // Print timestamp header matching psql's ctime-like format.
        let ts = format_system_time(std::time::SystemTime::now());
        println!("{ts} (every {interval_secs}s)\n");

        // Execute the stored query.  Use a fresh TxState so that
        // transaction state changes inside the loop are not persisted.
        let mut dummy_tx = TxState::default();
        execute_query(client, sql, settings, &mut dummy_tx).await;

        // Sleep for the interval, but exit cleanly on Ctrl-C.
        tokio::select! {
            () = sleep(Duration::from_secs_f64(interval_secs)) => {},
            _ = signal::ctrl_c() => {
                break;
            },
        }
    }
}

/// Parse a duration string like `"30s"`, `"5m"`, `"1h"`.
///
/// Returns `None` for invalid input.
pub(super) fn parse_duration(s: &str) -> Option<std::time::Duration> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let (num_str, multiplier) = if let Some(n) = s.strip_suffix('s') {
        (n, 1u64)
    } else if let Some(n) = s.strip_suffix('m') {
        (n, 60)
    } else if let Some(n) = s.strip_suffix('h') {
        (n, 3600)
    } else {
        // Bare number defaults to seconds.
        (s, 1)
    };
    let num: u64 = num_str.parse().ok()?;
    Some(std::time::Duration::from_secs(num * multiplier))
}

/// Run the observe loop — periodic database health snapshots.
///
/// Polls key diagnostic views every 10 seconds and prints a timestamped
/// summary.  Exits on Ctrl-C or when `duration_arg` elapses.  After
/// exiting, offers an AI-generated summary of the observation period.
#[allow(clippy::too_many_lines)]
pub(super) async fn observe_loop(
    client: &Client,
    settings: &mut ReplSettings,
    params: &ConnParams,
    duration_arg: Option<&str>,
) {
    use std::fmt::Write as _;
    use std::time::Duration;
    use tokio::signal;
    use tokio::time::sleep;

    let total_duration = duration_arg.and_then(parse_duration);

    if let Some(d) = total_duration {
        eprintln!(
            "-- Observing for {}s (Ctrl-C to stop early)...",
            d.as_secs()
        );
    } else {
        if duration_arg.is_some() {
            eprintln!("-- Invalid duration. Use e.g. \\observe 30s, \\observe 5m, \\observe 1h");
            return;
        }
        eprintln!("-- Observing (Ctrl-C to stop)...");
    }

    let start = std::time::Instant::now();
    let mut snapshots: Vec<String> = Vec::new();
    let interval = Duration::from_secs(10);
    let mut anomaly_detector = crate::anomaly::AnomalyDetector::new();

    loop {
        let ts = format_system_time(std::time::SystemTime::now());
        let mut report = format!("{ts} |");
        let mut metric_snap = crate::anomaly::MetricSnapshot::default();

        // 1. Connection count.
        if let Ok(rows) = client
            .simple_query(
                "SELECT count(*) FILTER (WHERE state = 'active') AS active, \
                 count(*) AS total \
                 FROM pg_stat_activity WHERE backend_type = 'client backend'",
            )
            .await
        {
            for msg in &rows {
                if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
                    let active = row.get(0).unwrap_or("?");
                    let total = row.get(1).unwrap_or("?");
                    let _ = write!(report, " connections: {active} active / {total} total");
                    metric_snap.active_sessions = active.parse().unwrap_or(0);
                    metric_snap.total_sessions = total.parse().unwrap_or(0);
                }
            }
        }

        // 2. Top wait event.
        if let Ok(rows) = client
            .simple_query(
                "SELECT wait_event_type || ':' || wait_event AS we, count(*) AS cnt \
                 FROM pg_stat_activity \
                 WHERE state = 'active' AND wait_event IS NOT NULL \
                 GROUP BY 1 ORDER BY 2 DESC LIMIT 1",
            )
            .await
        {
            for msg in &rows {
                if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
                    let we = row.get(0).unwrap_or("?");
                    let cnt = row.get(1).unwrap_or("?");
                    let _ = write!(report, " | top wait: {we} ({cnt})");
                    metric_snap.top_wait_count = cnt.parse().unwrap_or(0);
                }
            }
        }

        // 3. Long-running queries (> 30s).
        if let Ok(rows) = client
            .simple_query(
                "SELECT pid, \
                 extract(epoch FROM now() - query_start)::int AS secs, \
                 left(query, 60) AS q \
                 FROM pg_stat_activity \
                 WHERE state = 'active' \
                 AND query_start < now() - interval '30 seconds' \
                 AND backend_type = 'client backend' \
                 ORDER BY query_start LIMIT 3",
            )
            .await
        {
            let mut long_count = 0u32;
            for msg in &rows {
                if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
                    let pid = row.get(0).unwrap_or("?");
                    let secs = row.get(1).unwrap_or("?");
                    let q = row.get(2).unwrap_or("?");
                    let _ = write!(report, "\n  long query (pid {pid}, {secs}s): {q}");
                    long_count += 1;
                }
            }
            metric_snap.long_queries = long_count;
        }

        // 3b. Blocked sessions (for anomaly detection).
        if let Ok(rows) = client
            .simple_query(
                "SELECT count(*) FROM pg_stat_activity \
                 WHERE pid != pg_backend_pid() \
                 AND backend_type = 'client backend' \
                 AND wait_event_type = 'Lock'",
            )
            .await
        {
            for msg in &rows {
                if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
                    metric_snap.blocked_sessions =
                        row.get(0).and_then(|s| s.parse().ok()).unwrap_or(0);
                }
            }
        }

        // 4. Autovacuum activity.
        if let Ok(rows) = client
            .simple_query(
                "SELECT count(*) FROM pg_stat_activity \
                 WHERE backend_type = 'autovacuum worker'",
            )
            .await
        {
            for msg in &rows {
                if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
                    let cnt = row.get(0).unwrap_or("0");
                    if cnt != "0" {
                        let _ = write!(report, " | autovacuum workers: {cnt}");
                    }
                }
            }
        }

        // 5. Replication lag (if streaming replication is active).
        if let Ok(rows) = client
            .simple_query(
                "SELECT application_name, \
                 pg_wal_lsn_diff(pg_current_wal_lsn(), replay_lsn)::bigint AS lag_bytes \
                 FROM pg_stat_replication LIMIT 3",
            )
            .await
        {
            for msg in &rows {
                if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
                    let name = row.get(0).unwrap_or("?");
                    let lag = row.get(1).unwrap_or("?");
                    let _ = write!(report, " | repl lag ({name}): {lag} bytes");
                }
            }
        }

        eprintln!("{report}");
        snapshots.push(report);

        // Anomaly detection.
        let anomalies = anomaly_detector.check(&metric_snap);
        for anomaly in &anomalies {
            eprintln!(
                "  ** ANOMALY [{}]: {}",
                anomaly.kind.label(),
                anomaly.description
            );
        }
        if crate::anomaly::AnomalyDetector::should_trigger_rca(&anomalies) {
            eprintln!("  >> Auto-triggering RCA investigation...");
            let pg_ash_available = settings.db_capabilities.pg_ash.is_available();
            let snapshot = crate::rca::collect_snapshot(client, pg_ash_available).await;
            let data_steps = snapshot.steps.iter().filter(|s| s.has_data).count();
            eprintln!("  >> RCA: collected {data_steps} diagnostic steps.");
            print!("{}", snapshot.to_prompt());
            anomaly_detector.reset_rca_cooldown();
        }

        // Check if duration has elapsed.
        if let Some(d) = total_duration {
            if start.elapsed() >= d {
                break;
            }
        }

        // Sleep for the interval, exit on Ctrl-C.
        let remaining = total_duration.map(|d| d.saturating_sub(start.elapsed()));
        let sleep_time = match remaining {
            Some(r) if r < interval => r,
            _ => interval,
        };
        if sleep_time.is_zero() {
            break;
        }
        tokio::select! {
            () = sleep(sleep_time) => {},
            _ = signal::ctrl_c() => {
                break;
            },
        }
    }

    eprintln!("-- Observation ended ({} snapshots).", snapshots.len());

    // Offer AI summary if configured and we have data.
    if snapshots.is_empty() {
        return;
    }

    if settings
        .config
        .ai
        .provider
        .as_deref()
        .unwrap_or("")
        .is_empty()
    {
        return;
    }

    if !ask_yn_prompt("Generate AI summary? [Y/n] ", true) {
        return;
    }

    let provider = match get_ai_provider(settings) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("AI error: {e}");
            return;
        }
    };

    let observation_data = snapshots.join("\n");
    let system_content = format!(
        "You are a PostgreSQL expert analyzing database observation data.\n\
         Database: {dbname}\n\n\
         Rules:\n\
         - Summarize the key findings from the observation period\n\
         - Highlight any concerning patterns (connection pressure, long queries, lock contention)\n\
         - Provide actionable recommendations\n\
         - Be concise — this is a terminal report",
        dbname = params.dbname,
    );

    let messages = vec![
        crate::ai::Message {
            role: crate::ai::Role::System,
            content: system_content,
        },
        crate::ai::Message {
            role: crate::ai::Role::User,
            content: format!(
                "Here are the observation snapshots:\n\n{observation_data}\n\n\
                 Please summarize the findings and recommendations."
            ),
        },
    ];

    let options = crate::ai::CompletionOptions {
        model: settings.config.ai.model.clone().unwrap_or_default(),
        max_tokens: settings.config.ai.max_tokens,
        temperature: 0.0,
    };

    eprintln!("\n-- Summary:");
    match stream_completion(
        provider.as_ref(),
        &messages,
        &options,
        settings.no_highlight,
    )
    .await
    {
        Ok(result) => record_token_usage(settings, &result),
        Err(e) => eprintln!("AI error: {e}"),
    }
}
