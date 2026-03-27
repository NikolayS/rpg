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
 * Usage
 * -------------------------------------------------------------------------
 *
 *   node wasm/ws-proxy.js [options]
 *
 *   --pg-host <host>      Postgres TCP host    (default: 127.0.0.1, env: PG_HOST)
 *   --pg-port <port>      Postgres TCP port    (default: 5432,      env: PG_PORT)
 *   --listen-port <port>  WebSocket listen port(default: 9091,      env: PROXY_PORT)
 *   --listen-host <host>  WebSocket listen host(default: 127.0.0.1, env: PROXY_HOST)
 *
 * -------------------------------------------------------------------------
 * Protocol
 * -------------------------------------------------------------------------
 * - Binary WebSocket frames (not text).
 * - One TCP connection per WebSocket connection.
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
      case "--help":
      case "-h":
        console.error(
          "Usage: node ws-proxy.js " +
            "[--pg-host HOST] [--pg-port PORT] " +
            "[--listen-port PORT] [--listen-host HOST]"
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
    `-> ${config.pgHost}:${config.pgPort}`
);

wss.on("connection", (ws, req) => {
  const id = ++connId;
  const origin = req.headers.origin ?? req.socket.remoteAddress;
  console.error(`[ws-proxy] #${id} connected from ${origin}; opening TCP ${config.pgHost}:${config.pgPort}`);

  const tcp = net.createConnection({
    host: config.pgHost,
    port: config.pgPort,
  });

  let closed = false;

  // WebSocket binary frame -> TCP socket
  ws.on("message", (data) => {
    if (!tcp.destroyed) {
      tcp.write(data);
    }
  });

  // TCP socket -> WebSocket binary frame
  tcp.on("data", (data) => {
    if (ws.readyState === ws.OPEN) {
      ws.send(data);
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
    console.error(`[ws-proxy] #${id} tcp connected to ${config.pgHost}:${config.pgPort}`);
  });
});

// Graceful shutdown
process.on("SIGINT", () => {
  console.error("[ws-proxy] shutting down...");
  wss.close(() => process.exit(0));
});

process.on("SIGTERM", () => {
  console.error("[ws-proxy] shutting down...");
  wss.close(() => process.exit(0));
});
