//! Large object commands: `\lo_import`, `\lo_export`, `\lo_list`, `\lo_unlink`.
//!
//! Implements psql-compatible large object management using the `PostgreSQL`
//! server-side large object API (`lo_create`, `lo_open`, `loread`, `lowrite`,
//! `lo_close`, `lo_unlink`).
//!
//! All operations that mutate data (`\lo_import`, `\lo_unlink`) run inside an
//! explicit `begin` / `commit` block.  `\lo_export` also needs a transaction
//! because the large object API requires one to be open.
//!
//! # Chunk size
//!
//! Reads and writes use 64 KiB chunks — the same as psql.

use std::io::{Read, Write};
use std::path::Path;

use tokio_postgres::Client;

/// Chunk size for `loread` / `lowrite` calls (64 KiB, matching psql).
const CHUNK_SIZE: usize = 64 * 1024;

/// `lo_open` flag: read-write access.
const INV_READ_WRITE: i32 = 0x0002_0000 | 0x0004_0000; // 0x60000

/// `lo_open` flag: read-only access.
const INV_READ: i32 = 0x0004_0000; // 0x40000

// ---------------------------------------------------------------------------
// lo_import
// ---------------------------------------------------------------------------

/// Implement `\lo_import <filename> [<comment>]`.
///
/// Steps:
/// 1. Open and read the local file.
/// 2. Begin a transaction (if the connection is currently idle).
/// 3. Create a new large object (`lo_create(0)`) to obtain an OID.
/// 4. Open the large object with read-write access.
/// 5. Write the file contents in 64 KiB chunks via `lowrite`.
/// 6. Close the large object descriptor.
/// 7. Optionally set a comment on the object.
/// 8. Commit and print `lo_import <oid>`.
pub async fn lo_import(client: &Client, filename: &str, comment: &str) {
    // Read the file before touching the database so that a missing file
    // produces a clear error without starting a transaction.
    let data = match read_file(filename) {
        Ok(d) => d,
        Err(e) => {
            rpg_eprintln!("\\lo_import: {e}");
            return;
        }
    };

    if let Err(e) = run_lo_import(client, filename, comment, &data).await {
        rpg_eprintln!("\\lo_import: {e}");
    }
}

/// Inner async logic for `lo_import` — separated so we can use `?`.
async fn run_lo_import(
    client: &Client,
    _filename: &str,
    comment: &str,
    data: &[u8],
) -> Result<(), String> {
    // Begin transaction.
    simple_exec(client, "begin").await?;

    // Create a new large object and retrieve its OID.
    let oid = query_one_int(client, "select lo_create(0)").await?;

    // Open the large object for reading and writing.
    let fd = query_one_int(client, &format!("select lo_open({oid}, {INV_READ_WRITE})")).await?;

    // Write data in chunks.
    for chunk in data.chunks(CHUNK_SIZE) {
        let hex = hex_encode(chunk);
        simple_exec(client, &format!("select lowrite({fd}, '\\x{hex}'::bytea)")).await?;
    }

    // Close the large object descriptor.
    simple_exec(client, &format!("select lo_close({fd})")).await?;

    // Optionally set a comment.
    if !comment.is_empty() {
        let escaped = comment.replace('\'', "''");
        simple_exec(
            client,
            &format!("comment on large object {oid} is '{escaped}'"),
        )
        .await?;
    }

    // Commit.
    simple_exec(client, "commit").await?;

    rpg_println!("lo_import {oid}");
    Ok(())
}

// ---------------------------------------------------------------------------
// lo_export
// ---------------------------------------------------------------------------

/// Implement `\lo_export <loid> <filename>`.
///
/// Steps:
/// 1. Begin a transaction.
/// 2. Open the large object read-only.
/// 3. Read in 64 KiB chunks until `loread` returns an empty bytea.
/// 4. Close the descriptor.
/// 5. Commit.
/// 6. Write the accumulated bytes to the local file.
/// 7. Print `lo_export`.
pub async fn lo_export(client: &Client, loid: &str, filename: &str) {
    let Ok(loid_parsed) = loid.trim().parse::<u32>() else {
        rpg_eprintln!("\\lo_export: invalid OID \"{loid}\"");
        return;
    };

    match run_lo_export(client, loid_parsed, filename).await {
        Ok(()) => rpg_println!("lo_export"),
        Err(e) => rpg_eprintln!("\\lo_export: {e}"),
    }
}

async fn run_lo_export(client: &Client, loid: u32, filename: &str) -> Result<(), String> {
    simple_exec(client, "begin").await?;

    let fd = query_one_int(client, &format!("select lo_open({loid}, {INV_READ})")).await?;

    let mut buf: Vec<u8> = Vec::new();
    loop {
        let hex = query_one_str(client, &format!("select loread({fd}, {CHUNK_SIZE})")).await?;
        // Server returns `\x<hexdigits>` or an empty `\x`.
        let bytes = decode_bytea_hex(&hex)?;
        if bytes.is_empty() {
            break;
        }
        buf.extend_from_slice(&bytes);
    }

    simple_exec(client, &format!("select lo_close({fd})")).await?;
    simple_exec(client, "commit").await?;

    write_file(filename, &buf)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// lo_list  (\dl)
// ---------------------------------------------------------------------------

/// Implement `\lo_list` / `\dl`.
///
/// Queries `pg_largeobject_metadata` and prints the result as an aligned
/// table matching psql's output format.
pub async fn lo_list(client: &Client) {
    let sql = "\
        select \
            lom.oid as \"ID\", \
            pg_catalog.obj_description(lom.oid, 'pg_largeobject') as \"Description\" \
        from pg_catalog.pg_largeobject_metadata as lom \
        order by lom.oid";

    run_and_print(client, sql, Some("Large objects")).await;
}

// ---------------------------------------------------------------------------
// lo_unlink
// ---------------------------------------------------------------------------

/// Implement `\lo_unlink <loid>`.
pub async fn lo_unlink(client: &Client, loid: &str) {
    let Ok(loid_parsed) = loid.trim().parse::<u32>() else {
        rpg_eprintln!("\\lo_unlink: invalid OID \"{loid}\"");
        return;
    };

    match simple_exec(client, &format!("select lo_unlink({loid_parsed})")).await {
        Ok(()) => rpg_println!("lo_unlink {loid_parsed}"),
        Err(e) => rpg_eprintln!("\\lo_unlink: {e}"),
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Execute a statement and discard the result, returning `Err(msg)` on failure.
async fn simple_exec(client: &Client, sql: &str) -> Result<(), String> {
    client
        .simple_query(sql)
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Execute a query that returns exactly one integer cell.
async fn query_one_int(client: &Client, sql: &str) -> Result<i64, String> {
    use tokio_postgres::SimpleQueryMessage;

    let msgs = client.simple_query(sql).await.map_err(|e| e.to_string())?;

    for msg in msgs {
        if let SimpleQueryMessage::Row(row) = msg {
            let val = row.get(0).unwrap_or("");
            return val
                .parse::<i64>()
                .map_err(|_| format!("unexpected value from server: \"{val}\""));
        }
    }
    Err(format!("no row returned by: {sql}"))
}

/// Execute a query that returns exactly one text cell.
async fn query_one_str(client: &Client, sql: &str) -> Result<String, String> {
    use tokio_postgres::SimpleQueryMessage;

    let msgs = client.simple_query(sql).await.map_err(|e| e.to_string())?;

    for msg in msgs {
        if let SimpleQueryMessage::Row(row) = msg {
            return Ok(row.get(0).unwrap_or("").to_owned());
        }
    }
    Err(format!("no row returned by: {sql}"))
}

/// Execute `sql`, collect rows, and print a column-aligned table.
async fn run_and_print(client: &Client, sql: &str, title: Option<&str>) {
    use tokio_postgres::SimpleQueryMessage;

    match client.simple_query(sql).await {
        Ok(messages) => {
            let mut col_names: Vec<String> = Vec::new();
            let mut rows: Vec<Vec<String>> = Vec::new();

            for msg in messages {
                match msg {
                    SimpleQueryMessage::RowDescription(cols) => {
                        if col_names.is_empty() {
                            col_names = cols.iter().map(|c| c.name().to_owned()).collect();
                        }
                    }
                    SimpleQueryMessage::Row(row) => {
                        if col_names.is_empty() {
                            col_names = (0..row.len())
                                .map(|i| {
                                    row.columns()
                                        .get(i)
                                        .map_or_else(|| format!("col{i}"), |c| c.name().to_owned())
                                })
                                .collect();
                        }
                        let vals: Vec<String> = (0..row.len())
                            .map(|i| row.get(i).unwrap_or("").to_owned())
                            .collect();
                        rows.push(vals);
                    }
                    _ => {}
                }
            }

            print_table(&col_names, &rows, title);
        }
        Err(e) => {
            rpg_eprintln!("{e}");
        }
    }
}

/// Print a simple column-aligned table to stdout.
fn print_table(col_names: &[String], rows: &[Vec<String>], title: Option<&str>) {
    if col_names.is_empty() {
        let n = rows.len();
        let word = if n == 1 { "row" } else { "rows" };
        rpg_println!("({n} {word})");
        return;
    }

    let mut widths: Vec<usize> = col_names.iter().map(String::len).collect();
    for row in rows {
        for (i, val) in row.iter().enumerate() {
            if i < widths.len() {
                widths[i] = widths[i].max(val.len());
            }
        }
    }

    let ncols = widths.len();
    let table_width =
        1 + widths.iter().sum::<usize>() + if ncols > 1 { 3 * (ncols - 1) } else { 0 } + 1;

    if let Some(t) = title {
        let tlen = t.len();
        if tlen >= table_width {
            rpg_println!("{t}");
        } else {
            let pad = (table_width - tlen) / 2;
            rpg_println!("{:pad$}{t}", "");
        }
    }

    // Header row.
    let header: String = col_names
        .iter()
        .enumerate()
        .map(|(i, name)| format!(" {name:<w$}", w = widths[i]))
        .collect::<Vec<_>>()
        .join(" |");
    rpg_println!("{header}");

    // Separator.
    let sep: String = widths
        .iter()
        .map(|&w| "-".repeat(w + 2))
        .collect::<Vec<_>>()
        .join("+");
    rpg_println!("{sep}");

    // Data rows.
    for row in rows {
        let line: String = row
            .iter()
            .enumerate()
            .map(|(i, val)| {
                let w = widths.get(i).copied().unwrap_or(0);
                format!(" {val:<w$}")
            })
            .collect::<Vec<_>>()
            .join(" |");
        rpg_println!("{line}");
    }

    let n = rows.len();
    let word = if n == 1 { "row" } else { "rows" };
    rpg_println!("({n} {word})");
}

/// Read the entire contents of a local file.
fn read_file(path: &str) -> Result<Vec<u8>, String> {
    let mut f =
        std::fs::File::open(Path::new(path)).map_err(|e| format!("cannot open \"{path}\": {e}"))?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)
        .map_err(|e| format!("cannot read \"{path}\": {e}"))?;
    Ok(buf)
}

/// Write bytes to a local file (creates or truncates).
fn write_file(path: &str, data: &[u8]) -> Result<(), String> {
    let mut f = std::fs::File::create(Path::new(path))
        .map_err(|e| format!("cannot create \"{path}\": {e}"))?;
    f.write_all(data)
        .map_err(|e| format!("cannot write \"{path}\": {e}"))?;
    Ok(())
}

/// Encode a byte slice as lowercase hex digits.
fn hex_encode(data: &[u8]) -> String {
    use std::fmt::Write as _;
    data.iter()
        .fold(String::with_capacity(data.len() * 2), |mut s, b| {
            write!(s, "{b:02x}").unwrap();
            s
        })
}

/// Decode the `\x<hexdigits>` bytea text representation returned by `PostgreSQL`.
///
/// Returns an empty `Vec` when `s` is `\x` (empty bytea).
fn decode_bytea_hex(s: &str) -> Result<Vec<u8>, String> {
    let hex = s
        .strip_prefix("\\x")
        .ok_or_else(|| format!("unexpected bytea format: \"{s}\""))?;

    if hex.is_empty() {
        return Ok(Vec::new());
    }

    if hex.len() % 2 != 0 {
        return Err(format!("odd-length hex string from server: \"{s}\""));
    }

    hex.as_bytes()
        .chunks(2)
        .map(|pair| {
            let hi = hex_digit(pair[0])?;
            let lo = hex_digit(pair[1])?;
            Ok((hi << 4) | lo)
        })
        .collect()
}

fn hex_digit(b: u8) -> Result<u8, String> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        b'A'..=b'F' => Ok(b - b'A' + 10),
        _ => Err(format!("invalid hex digit: '{}'", char::from(b))),
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- hex_encode ----------------------------------------------------------

    #[test]
    fn hex_encode_empty_slice() {
        assert_eq!(hex_encode(&[]), "");
    }

    #[test]
    fn hex_encode_single_zero_byte() {
        assert_eq!(hex_encode(&[0x00]), "00");
    }

    #[test]
    fn hex_encode_single_ff_byte() {
        assert_eq!(hex_encode(&[0xff]), "ff");
    }

    #[test]
    fn hex_encode_well_known_bytes() {
        assert_eq!(hex_encode(&[0xde, 0xad, 0xbe, 0xef]), "deadbeef");
    }

    #[test]
    fn hex_encode_output_is_always_lowercase() {
        let result = hex_encode(&[0xab, 0xcd, 0xef]);
        assert_eq!(result, result.to_lowercase(), "hex must be lowercase");
    }

    #[test]
    fn hex_encode_length_is_twice_input() {
        let data: Vec<u8> = (0u8..=255).collect();
        let encoded = hex_encode(&data);
        assert_eq!(encoded.len(), 512, "each byte becomes 2 hex chars");
    }

    #[test]
    fn hex_encode_single_digit_values_are_zero_padded() {
        assert_eq!(hex_encode(&[0x01]), "01");
        assert_eq!(hex_encode(&[0x0f]), "0f");
    }

    // -- hex_digit -----------------------------------------------------------

    #[test]
    fn hex_digit_numeric_digits_0_through_9() {
        for (b, expected) in (b'0'..=b'9').zip(0u8..=9) {
            assert_eq!(hex_digit(b), Ok(expected), "digit '{}'", char::from(b));
        }
    }

    #[test]
    fn hex_digit_lowercase_a_through_f() {
        for (b, expected) in (b'a'..=b'f').zip(10u8..=15) {
            assert_eq!(hex_digit(b), Ok(expected), "digit '{}'", char::from(b));
        }
    }

    #[test]
    fn hex_digit_uppercase_a_through_f() {
        for (b, expected) in (b'A'..=b'F').zip(10u8..=15) {
            assert_eq!(hex_digit(b), Ok(expected), "digit '{}'", char::from(b));
        }
    }

    #[test]
    fn hex_digit_g_lowercase_returns_err() {
        assert!(hex_digit(b'g').is_err());
    }

    #[test]
    fn hex_digit_g_uppercase_returns_err() {
        assert!(hex_digit(b'G').is_err());
    }

    #[test]
    fn hex_digit_space_returns_err() {
        assert!(hex_digit(b' ').is_err());
    }

    #[test]
    fn hex_digit_special_chars_return_err() {
        for &b in b"!@#$%^&*()_+-=" {
            assert!(
                hex_digit(b).is_err(),
                "expected Err for '{}'",
                char::from(b)
            );
        }
    }

    // -- decode_bytea_hex ----------------------------------------------------

    #[test]
    fn decode_bytea_hex_empty_bytea() {
        assert_eq!(decode_bytea_hex("\\x"), Ok(vec![]));
    }

    #[test]
    fn decode_bytea_hex_single_byte_zero() {
        assert_eq!(decode_bytea_hex("\\x00"), Ok(vec![0x00]));
    }

    #[test]
    fn decode_bytea_hex_single_byte_ff() {
        assert_eq!(decode_bytea_hex("\\xff"), Ok(vec![0xff]));
    }

    #[test]
    fn decode_bytea_hex_multi_byte_deadbeef() {
        assert_eq!(
            decode_bytea_hex("\\xdeadbeef"),
            Ok(vec![0xde, 0xad, 0xbe, 0xef]),
        );
    }

    #[test]
    fn decode_bytea_hex_uppercase_digits_accepted() {
        assert_eq!(decode_bytea_hex("\\xDEAD"), Ok(vec![0xde, 0xad]));
    }

    #[test]
    fn decode_bytea_hex_mixed_case_accepted() {
        assert_eq!(decode_bytea_hex("\\xDeAd"), Ok(vec![0xde, 0xad]));
    }

    #[test]
    fn decode_bytea_hex_missing_prefix_returns_err() {
        assert!(decode_bytea_hex("deadbeef").is_err());
        assert!(decode_bytea_hex("ff").is_err());
    }

    #[test]
    fn decode_bytea_hex_wrong_0x_prefix_returns_err() {
        assert!(decode_bytea_hex("0xdeadbeef").is_err());
    }

    #[test]
    fn decode_bytea_hex_odd_length_returns_err() {
        assert!(
            decode_bytea_hex("\\xabc").is_err(),
            "odd-length hex must be an error"
        );
        assert!(decode_bytea_hex("\\xf").is_err());
    }

    #[test]
    fn decode_bytea_hex_invalid_digit_returns_err() {
        assert!(decode_bytea_hex("\\xgg").is_err());
        assert!(decode_bytea_hex("\\xzz").is_err());
    }

    // -- roundtrip -----------------------------------------------------------

    #[test]
    fn hex_encode_decode_roundtrip_all_byte_values() {
        let data: Vec<u8> = (0u8..=255).collect();
        let hex_str = format!("\\x{}", hex_encode(&data));
        let decoded = decode_bytea_hex(&hex_str).expect("roundtrip decode must succeed");
        assert_eq!(
            decoded, data,
            "roundtrip must be identity for all byte values"
        );
    }

    #[test]
    fn hex_encode_decode_roundtrip_empty() {
        let hex_str = format!("\\x{}", hex_encode(&[]));
        let decoded = decode_bytea_hex(&hex_str).expect("empty roundtrip");
        assert_eq!(decoded, Vec::<u8>::new());
    }
}
