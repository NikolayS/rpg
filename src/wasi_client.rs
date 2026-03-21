//! WASI WebSocket PG client — sync, tungstenite-based.
//!
//! Connects to a Postgres WSS proxy over WebSocket, performs the PG wire
//! protocol startup sequence, executes queries, and prints results.
//!
//! Copyright 2026 Postgres.ai

use std::collections::VecDeque;
use std::env;
use tungstenite::{connect, Message};

// ---------------------------------------------------------------------------
// Config / CLI parsing
// ---------------------------------------------------------------------------

struct Config {
    ws_url: String,
    user: String,
    password: String,
    database: String,
    commands: Vec<String>,
}

fn parse_config() -> Config {
    let ws_url = env::var("PG_WSS_URL").unwrap_or_default();
    let mut user = env::var("PGUSER").unwrap_or_else(|_| "postgres".to_owned());
    let password = env::var("PGPASSWORD").unwrap_or_default();
    let mut database = env::var("PGDATABASE").unwrap_or_default();
    let mut commands: Vec<String> = Vec::new();

    // Parse CLI args: -U user, -d database, -h host, -p port, -c SQL
    let args: Vec<String> = env::args().collect();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-U" | "--username" => {
                i += 1;
                if i < args.len() {
                    user.clone_from(&args[i]);
                }
            }
            "-d" | "--dbname" => {
                i += 1;
                if i < args.len() {
                    database.clone_from(&args[i]);
                }
            }
            "-h" | "--host" | "-p" | "--port" => {
                i += 1; // consume value; URL already encodes host/port
            }
            "-c" | "--command" => {
                i += 1;
                if i < args.len() {
                    commands.push(args[i].clone());
                }
            }
            arg if arg.starts_with("-U") => {
                arg[2..].clone_into(&mut user);
            }
            arg if arg.starts_with("-d") => {
                arg[2..].clone_into(&mut database);
            }
            arg if arg.starts_with("-c") => {
                commands.push(arg[2..].to_owned());
            }
            _ => {}
        }
        i += 1;
    }

    if ws_url.is_empty() {
        eprintln!("rpg: PG_WSS_URL is not set");
        std::process::exit(1);
    }

    if database.is_empty() {
        database.clone_from(&user);
    }

    Config {
        ws_url,
        user,
        password,
        database,
        commands,
    }
}

// ---------------------------------------------------------------------------
// PG wire protocol helpers
// ---------------------------------------------------------------------------

/// Convert a `usize` to `u32`, exiting on overflow.
fn len_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or_else(|_| {
        eprintln!("rpg: message body too large ({n} bytes)");
        std::process::exit(1);
    })
}

/// Build a `StartupMessage`: `length(4) + protocol_version(4) + params + NUL`
fn startup_message(user: &str, database: &str) -> Vec<u8> {
    let mut payload: Vec<u8> = Vec::new();
    // Protocol version 3.0
    payload.extend_from_slice(&[0x00, 0x03, 0x00, 0x00]);
    payload.extend_from_slice(b"user\0");
    payload.extend_from_slice(user.as_bytes());
    payload.push(0);
    payload.extend_from_slice(b"database\0");
    payload.extend_from_slice(database.as_bytes());
    payload.push(0);
    // Trailing terminator
    payload.push(0);

    // Length field includes itself (4 bytes).
    let len = (4u32 + len_u32(payload.len())).to_be_bytes();
    let mut msg: Vec<u8> = Vec::with_capacity(4 + payload.len());
    msg.extend_from_slice(&len);
    msg.extend_from_slice(&payload);
    msg
}

/// Build a frontend message: `type_byte + length(4) + body`
fn pg_message(type_byte: u8, body: &[u8]) -> Vec<u8> {
    let len = (4u32 + len_u32(body.len())).to_be_bytes();
    let mut msg = Vec::with_capacity(1 + 4 + body.len());
    msg.push(type_byte);
    msg.extend_from_slice(&len);
    msg.extend_from_slice(body);
    msg
}

/// Build a `Query` message: `'Q' + length(4) + sql + NUL`
fn query_message(sql: &str) -> Vec<u8> {
    let mut body = sql.as_bytes().to_vec();
    body.push(0);
    pg_message(b'Q', &body)
}

/// Build a `PasswordMessage`: `'p' + length(4) + password + NUL`
fn password_message(password: &str) -> Vec<u8> {
    let mut body = password.as_bytes().to_vec();
    body.push(0);
    pg_message(b'p', &body)
}

/// Build a `Terminate` message: `'X' + length(4)`
fn terminate_message() -> Vec<u8> {
    pg_message(b'X', &[])
}

// ---------------------------------------------------------------------------
// PG backend message types
// ---------------------------------------------------------------------------

enum PgMsg {
    AuthOk,
    AuthCleartextPassword,
    AuthUnknown(i32),
    ParameterStatus,
    BackendKeyData,
    ReadyForQuery,
    RowDescription(Vec<String>),
    DataRow(Vec<Option<String>>),
    CommandComplete(String),
    ErrorResponse(String),
    NoticeResponse,
    Unknown,
}

// ---------------------------------------------------------------------------
// WebSocket connection with buffered PG reader
// ---------------------------------------------------------------------------

struct WsConn {
    ws: tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>,
    buf: VecDeque<u8>,
}

impl WsConn {
    fn send(&mut self, bytes: &[u8]) {
        if let Err(e) = self
            .ws
            .send(Message::Binary(bytes::Bytes::copy_from_slice(bytes)))
        {
            eprintln!("rpg: websocket send error: {e}");
            std::process::exit(1);
        }
    }

    /// Pull WebSocket frames until `buf` has at least `n` bytes.
    fn fill(&mut self, n: usize) {
        while self.buf.len() < n {
            match self.ws.read() {
                Ok(Message::Binary(data)) => {
                    self.buf.extend(data.iter());
                }
                Ok(Message::Text(t)) => {
                    eprintln!("rpg: unexpected text frame: {t}");
                    std::process::exit(1);
                }
                Ok(Message::Ping(d)) => {
                    let _ = self.ws.send(Message::Pong(d));
                }
                Ok(Message::Close(_)) => {
                    eprintln!("rpg: server closed connection unexpectedly");
                    std::process::exit(1);
                }
                Ok(_) => {}
                Err(e) => {
                    eprintln!("rpg: websocket read error: {e}");
                    std::process::exit(1);
                }
            }
        }
    }

    fn read_byte(&mut self) -> u8 {
        self.fill(1);
        self.buf.pop_front().unwrap_or(0)
    }

    fn read_i32(&mut self) -> i32 {
        self.fill(4);
        let b = [
            self.buf.pop_front().unwrap_or(0),
            self.buf.pop_front().unwrap_or(0),
            self.buf.pop_front().unwrap_or(0),
            self.buf.pop_front().unwrap_or(0),
        ];
        i32::from_be_bytes(b)
    }

    fn read_bytes(&mut self, n: usize) -> Vec<u8> {
        self.fill(n);
        (0..n).map(|_| self.buf.pop_front().unwrap_or(0)).collect()
    }

    /// Read one PG backend message (post-startup: type byte + length + payload).
    fn read_msg(&mut self) -> PgMsg {
        let type_byte = self.read_byte();
        let len = self.read_i32();
        let payload_len = usize::try_from((len - 4).max(0)).unwrap_or(0);
        let payload = self.read_bytes(payload_len);

        match type_byte {
            b'R' => parse_auth(&payload),
            b'S' => PgMsg::ParameterStatus,
            b'K' => PgMsg::BackendKeyData,
            b'Z' => PgMsg::ReadyForQuery,
            b'T' => parse_row_description(&payload),
            b'D' => parse_data_row(&payload),
            b'C' => {
                let tag = String::from_utf8_lossy(&payload)
                    .trim_end_matches('\0')
                    .to_owned();
                PgMsg::CommandComplete(tag)
            }
            b'E' => PgMsg::ErrorResponse(extract_error_message(&payload)),
            b'N' => PgMsg::NoticeResponse,
            _ => PgMsg::Unknown,
        }
    }
}

fn parse_auth(payload: &[u8]) -> PgMsg {
    if payload.len() < 4 {
        return PgMsg::AuthUnknown(-1);
    }
    let code = i32::from_be_bytes([payload[0], payload[1], payload[2], payload[3]]);
    match code {
        0 => PgMsg::AuthOk,
        3 => PgMsg::AuthCleartextPassword,
        _ => PgMsg::AuthUnknown(code),
    }
}

fn parse_row_description(payload: &[u8]) -> PgMsg {
    if payload.len() < 2 {
        return PgMsg::RowDescription(vec![]);
    }
    let nfields = u16::from_be_bytes([payload[0], payload[1]]) as usize;
    let mut fields = Vec::with_capacity(nfields);
    let mut pos = 2;
    for _ in 0..nfields {
        let start = pos;
        while pos < payload.len() && payload[pos] != 0 {
            pos += 1;
        }
        fields.push(String::from_utf8_lossy(&payload[start..pos]).into_owned());
        pos += 1; // skip null terminator
                  // skip tableOID(4) + colAttr(2) + typeOID(4) + typeSize(2) + typeMod(4) + format(2) = 18
        pos += 18;
    }
    PgMsg::RowDescription(fields)
}

fn parse_data_row(payload: &[u8]) -> PgMsg {
    if payload.len() < 2 {
        return PgMsg::DataRow(vec![]);
    }
    let ncols = u16::from_be_bytes([payload[0], payload[1]]) as usize;
    let mut values = Vec::with_capacity(ncols);
    let mut pos = 2;
    for _ in 0..ncols {
        if pos + 4 > payload.len() {
            values.push(None);
            continue;
        }
        let col_len = i32::from_be_bytes([
            payload[pos],
            payload[pos + 1],
            payload[pos + 2],
            payload[pos + 3],
        ]);
        pos += 4;
        if col_len == -1 {
            values.push(None);
        } else {
            let len = usize::try_from(col_len).unwrap_or(0);
            if pos + len > payload.len() {
                values.push(None);
            } else {
                let s = String::from_utf8_lossy(&payload[pos..pos + len]).into_owned();
                values.push(Some(s));
                pos += len;
            }
        }
    }
    PgMsg::DataRow(values)
}

fn extract_error_message(payload: &[u8]) -> String {
    let mut pos = 0;
    while pos < payload.len() {
        let ftype = payload[pos];
        pos += 1;
        if ftype == 0 {
            break;
        }
        let start = pos;
        while pos < payload.len() && payload[pos] != 0 {
            pos += 1;
        }
        if ftype == b'M' {
            return String::from_utf8_lossy(&payload[start..pos]).into_owned();
        }
        pos += 1; // skip null terminator
    }
    "unknown error".to_owned()
}

// ---------------------------------------------------------------------------
// Connection and query execution
// ---------------------------------------------------------------------------

fn ws_connect(url: &str) -> WsConn {
    match connect(url) {
        Ok((ws, _)) => WsConn {
            ws,
            buf: VecDeque::new(),
        },
        Err(e) => {
            eprintln!("rpg: failed to connect to {url}: {e}");
            std::process::exit(1);
        }
    }
}

fn do_startup(conn: &mut WsConn, user: &str, database: &str, password: &str) {
    conn.send(&startup_message(user, database));

    loop {
        match conn.read_msg() {
            PgMsg::AuthCleartextPassword => {
                conn.send(&password_message(password));
            }
            PgMsg::AuthUnknown(code) => {
                eprintln!("rpg: unsupported auth method (code {code})");
                std::process::exit(1);
            }
            PgMsg::ErrorResponse(msg) => {
                eprintln!("rpg: {msg}");
                std::process::exit(1);
            }
            PgMsg::ReadyForQuery => break,
            _ => {} // AuthOk, ParameterStatus, BackendKeyData, etc.
        }
    }
}

fn execute_query(conn: &mut WsConn, sql: &str) {
    conn.send(&query_message(sql));

    let mut row_count = 0usize;
    let mut had_row_desc = false;

    loop {
        match conn.read_msg() {
            PgMsg::RowDescription(cols) => {
                println!("{}", cols.join("\t"));
                had_row_desc = true;
            }
            PgMsg::DataRow(values) => {
                let row: Vec<String> = values
                    .into_iter()
                    .map(|v| v.unwrap_or_else(|| "NULL".to_owned()))
                    .collect();
                println!("{}", row.join("\t"));
                row_count += 1;
            }
            PgMsg::CommandComplete(tag) => {
                if had_row_desc {
                    println!("({row_count} row{})", if row_count == 1 { "" } else { "s" });
                } else {
                    println!("{tag}");
                }
            }
            PgMsg::ReadyForQuery => break,
            PgMsg::ErrorResponse(msg) => {
                eprintln!("ERROR:  {msg}");
                // keep looping until ReadyForQuery
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

pub fn run() {
    let cfg = parse_config();
    let mut conn = ws_connect(&cfg.ws_url);
    do_startup(&mut conn, &cfg.user, &cfg.database, &cfg.password);

    for sql in &cfg.commands {
        execute_query(&mut conn, sql);
    }

    conn.send(&terminate_message());
}
