#!/usr/bin/env node

/**
 * ws-proxy.js — WebSocket-to-TCP proxy for rpg WASM
 *
 * Copyright 2026 Nikolay Samokhvalov / postgres.ai
 * SPDX-License-Identifier: Apache-2.0
 *
 * -------------------------------------------------------------------------
 * Overview
 * -------------------------------------------------------------------------
 * When rpg.wasm runs in the browser it cannot open TCP sockets directly.
 * Instead, the WasmConnector (src/wasm/connector.rs) opens a WebSocket to
 * this proxy, which then opens a real TCP connection to Postgres and relays
 * binary frames in both directions.
 *
 * This also works with the Emscripten build path, where POSIX `connect()`
 * syscalls are transparently mapped to WebSocket connections.
 *
 * -------------------------------------------------------------------------
 * Authentication
 * -------------------------------------------------------------------------
 * When --token <secret> (or WS_PROXY_TOKEN env var) is set, every incoming
 * WebSocket connection must send a JSON auth frame as its first message:
 *
 *   {"token": "<secret>"}
 *
 * If the token does not match, the connection is closed with code 4001
 * (Unauthorized).  If the first message is not valid JSON, code 4002
 * (Invalid auth frame) is used.
 *
 * When no token is configured the proxy runs unauthenticated — acceptable
 * for local development only.  A warning is logged to stderr on startup.
 *
 * -------------------------------------------------------------------------
 * Usage
 * -------------------------------------------------------------------------
 *
 *   node wasm/ws-proxy.js [options]
 *
 *   --pg-host <host>      Postgres TCP host    (default: 127.0.0.1, env: PG_HOST)
 *   --pg-port <port>      Postgres TCP port    (default: 5432,      env: PG_PORT)
 *   --listen-port <port>  WebSocket listen port(default: 9091,      env: PROXY_PORT)
 *   --listen-host <host>  WebSocket listen host(default: 127.0.0.1, env: PROXY_HOST)
 *   --token <secret>      Auth token           (env: WS_PROXY_TOKEN)
 *
 * -------------------------------------------------------------------------
 * Protocol
 * -------------------------------------------------------------------------
 * - When a token is configured, the first WS message must be a JSON auth
 *   frame: {"token": "..."}. Subsequent messages are binary Postgres data.
 * - When no token is configured, all messages are binary Postgres data.
 * - One TCP connection per WebSocket connection.
 * - Backpressure: TCP reads are paused when the WS send buffer exceeds a
 *   high-water mark, and resumed on drain.  Similarly, WS reads are paused
 *   when the TCP write buffer is full, and resumed on drain.
 * - Clean close propagation in both directions.
 * - All diagnostic output goes to stderr.
 */

"use strict";

const net = require("net");
const { WebSocketServer } = require("ws");

// ---------------------------------------------------------------------------
// Argument parsing
// ---------------------------------------------------------------------------

function parseArgs(argv) {
  const args = {
    pgHost: process.env.PG_HOST ?? "127.0.0.1",
    pgPort: parseInt(process.env.PG_PORT ?? "5432", 10),
    listenPort: parseInt(process.env.PROXY_PORT ?? "9091", 10),
    listenHost: process.env.PROXY_HOST ?? "127.0.0.1",
    token: process.env.WS_PROXY_TOKEN ?? null,
  };

  for (let i = 2; i < argv.length; i++) {
    switch (argv[i]) {
      case "--pg-host":
        args.pgHost = argv[++i];
        break;
      case "--pg-port":
        args.pgPort = parseInt(argv[++i], 10);
        break;
      case "--listen-port":
        args.listenPort = parseInt(argv[++i], 10);
        break;
      case "--listen-host":
        args.listenHost = argv[++i];
        break;
      case "--token":
        args.token = argv[++i];
        break;
      case "--help":
      case "-h":
        console.error(
          "Usage: node ws-proxy.js " +
            "[--pg-host HOST] [--pg-port PORT] " +
            "[--listen-port PORT] [--listen-host HOST] " +
            "[--token SECRET]"
        );
        process.exit(0);
        break;
      default:
        console.error(`Unknown argument: ${argv[i]}`);
        process.exit(1);
    }
  }

  return args;
}

const config = parseArgs(process.argv);

if (!config.token) {
  console.error(
    "[ws-proxy] WARNING: no --token or WS_PROXY_TOKEN set — proxy is " +
      "unauthenticated. This is acceptable for local development only. " +
      "Set a token for any network-exposed deployment."
  );
}

// ---------------------------------------------------------------------------
// Connection counter for log context
// ---------------------------------------------------------------------------

let connId = 0;

// ---------------------------------------------------------------------------
// WebSocket server
// ---------------------------------------------------------------------------

const wss = new WebSocketServer({
  host: config.listenHost,
  port: config.listenPort,
});

console.error(
  `[ws-proxy] listening on ws://${config.listenHost}:${config.listenPort} ` +
    `-> ${config.pgHost}:${config.pgPort}` +
    (config.token ? " (auth enabled)" : " (no auth)")
);

wss.on("connection", (ws, req) => {
  const id = ++connId;
  const origin = req.headers.origin ?? req.socket.remoteAddress;
  console.error(`[ws-proxy] #${id} connected from ${origin}`);

  // -----------------------------------------------------------------------
  // Authentication gate
  // -----------------------------------------------------------------------
  // When a token is configured, the first message must be a JSON auth
  // frame: {"token": "..."}. Only after successful auth do we open the TCP
  // connection and start relaying.  When no token is configured, we skip
  // the auth step and start relaying immediately.

  if (config.token) {
    ws.once("message", (data) => {
      try {
        const msg = JSON.parse(data.toString());
        if (msg.token !== config.token) {
          console.error(`[ws-proxy] #${id} auth failed — bad token`);
          ws.close(4001, "Unauthorized");
          return;
        }
      } catch {
        console.error(`[ws-proxy] #${id} auth failed — invalid JSON`);
        ws.close(4002, "Invalid auth frame");
        return;
      }

      console.error(`[ws-proxy] #${id} auth ok — opening TCP`);
      startRelay(id, ws);
    });
  } else {
    startRelay(id, ws);
  }
});

// ---------------------------------------------------------------------------
// Backpressure tuning
// ---------------------------------------------------------------------------

// When the WebSocket bufferedAmount exceeds this threshold, pause TCP reads
// until the WS drains.  64 KiB is a conservative default that prevents
// unbounded memory growth under load while keeping latency low for normal
// interactive use.
const WS_HIGH_WATER_MARK = 64 * 1024;

// ---------------------------------------------------------------------------
// Relay logic (post-auth)
// ---------------------------------------------------------------------------

function startRelay(id, ws) {
  console.error(
    `[ws-proxy] #${id} opening TCP ${config.pgHost}:${config.pgPort}`
  );

  const tcp = net.createConnection({
    host: config.pgHost,
    port: config.pgPort,
  });

  let closed = false;

  // WS -> TCP with backpressure.
  // If the TCP write buffer is full, pause incoming WS reads until TCP
  // drains.  This prevents unbounded buffering when the WS client sends
  // data faster than the Postgres TCP socket can consume it.
  ws.on("message", (data) => {
    if (tcp.destroyed) return;
    const canWrite = tcp.write(data);
    if (!canWrite) {
      ws._socket.pause();
      tcp.once("drain", () => {
        if (ws.readyState === ws.OPEN) {
          ws._socket.resume();
        }
      });
    }
  });

  // TCP -> WS with backpressure.
  // If the WS send buffer exceeds the high-water mark, pause TCP reads
  // until the WS drains.  This prevents unbounded memory growth when
  // Postgres sends data faster than the WS client can consume it.
  tcp.on("data", (data) => {
    if (ws.readyState !== ws.OPEN) return;
    ws.send(data, { binary: true }, (err) => {
      if (err) {
        console.error(`[ws-proxy] #${id} WS send error: ${err.message}`);
      }
    });
    if (ws.bufferedAmount > WS_HIGH_WATER_MARK) {
      tcp.pause();
      ws.once("drain", () => {
        if (!tcp.destroyed) {
          tcp.resume();
        }
      });
    }
  });

  // Clean close propagation both ways.
  const cleanup = (label) => () => {
    if (closed) return;
    closed = true;
    console.error(`[ws-proxy] #${id} ${label} — tearing down`);
    if (!tcp.destroyed) tcp.destroy();
    if (ws.readyState !== ws.CLOSED && ws.readyState !== ws.CLOSING) {
      ws.terminate();
    }
  };

  ws.on("close", cleanup("ws closed"));
  ws.on("error", (err) => {
    console.error(`[ws-proxy] #${id} ws error: ${err.message}`);
    cleanup("ws error")();
  });

  tcp.on("close", cleanup("tcp closed"));
  tcp.on("error", (err) => {
    console.error(`[ws-proxy] #${id} tcp error: ${err.message}`);
    cleanup("tcp error")();
  });

  tcp.on("connect", () => {
    console.error(
      `[ws-proxy] #${id} tcp connected to ${config.pgHost}:${config.pgPort}`
    );
  });
}

// Graceful shutdown
process.on("SIGINT", () => {
  console.error("[ws-proxy] shutting down...");
  wss.close(() => process.exit(0));
});

process.on("SIGTERM", () => {
  console.error("[ws-proxy] shutting down...");
  wss.close(() => process.exit(0));
});
