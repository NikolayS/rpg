/**
 * ws-proxy.js — WebSocket-to-TCP proxy for rpg WASM
 *
 * Copyright 2026 Nikolay Samokhvalov / postgres.ai
 * SPDX-License-Identifier: Apache-2.0
 *
 * -------------------------------------------------------------------------
 * Overview
 * -------------------------------------------------------------------------
 * Emscripten's network emulation maps POSIX `connect()` syscalls to a
 * WebSocket connection.  When rpg.wasm tries to open a TCP socket to a
 * Postgres host it actually connects to a WebSocket URL baked in at
 * link time (default: ws://127.0.0.1:9090).
 *
 * This file documents and provides a reference proxy that:
 *   1. Accepts WebSocket connections from the browser (rpg.wasm).
 *   2. Opens a real TCP connection to the target Postgres host.
 *   3. Relays binary frames in both directions.
 *
 * The same proxy pattern is used by the psql WASM port (see
 * wasm/build-psql-wasm.sh for the Álvaro Herrera / Emscripten approach).
 *
 * -------------------------------------------------------------------------
 * Quick start (Node.js ≥ 18)
 * -------------------------------------------------------------------------
 *
 *   npm install ws          # or: node --experimental-require-module
 *   node wasm/ws-proxy.js
 *
 * Then open the browser page that loads rpg.js.  rpg.wasm will connect to
 * ws://127.0.0.1:9090 and the proxy will forward traffic to the Postgres
 * instance configured via environment variables.
 *
 * -------------------------------------------------------------------------
 * Environment variables
 * -------------------------------------------------------------------------
 *   PROXY_PORT   — WebSocket listen port   (default: 9090)
 *   PROXY_HOST   — WebSocket listen host   (default: 127.0.0.1)
 *   PG_HOST      — Postgres TCP host       (default: 127.0.0.1)
 *   PG_PORT      — Postgres TCP port       (default: 5432)
 *
 * -------------------------------------------------------------------------
 * TODO: Rust-side WebSocket connector
 * -------------------------------------------------------------------------
 * The current rpg WASM build relies entirely on Emscripten's transparent
 * socket-to-WebSocket remapping.  A future improvement is to implement a
 * native Rust WebSocket connector so that:
 *   - The WS URL can be set at runtime (not only at link time).
 *   - Multiple simultaneous Postgres connections can be multiplexed.
 *   - The proxy protocol can carry connection metadata (host, port) so a
 *     single proxy can route to multiple Postgres instances.
 *
 * See connection.rs for where the custom AsyncRead/AsyncWrite connector
 * would be plugged in for the wasm32 target.
 */

"use strict";

const net = require("net");
const { WebSocketServer } = require("ws");

const PROXY_PORT = parseInt(process.env.PROXY_PORT ?? "9090", 10);
const PROXY_HOST = process.env.PROXY_HOST ?? "127.0.0.1";
const PG_HOST    = process.env.PG_HOST    ?? "127.0.0.1";
const PG_PORT    = parseInt(process.env.PG_PORT ?? "5432", 10);

const wss = new WebSocketServer({ host: PROXY_HOST, port: PROXY_PORT });

console.log(
  `[ws-proxy] Listening on ws://${PROXY_HOST}:${PROXY_PORT} ` +
  `→ ${PG_HOST}:${PG_PORT}`
);

wss.on("connection", (ws) => {
  console.log(`[ws-proxy] Browser connected; opening TCP ${PG_HOST}:${PG_PORT}`);

  const tcp = net.createConnection({ host: PG_HOST, port: PG_PORT });

  // WebSocket frame (binary) → TCP socket
  ws.on("message", (data) => {
    if (!tcp.destroyed) tcp.write(data);
  });

  // TCP socket → WebSocket frame (binary)
  tcp.on("data", (data) => {
    if (ws.readyState === ws.OPEN) ws.send(data);
  });

  // Tear down both sides on any error or close
  const cleanup = (label) => () => {
    console.log(`[ws-proxy] ${label} closed — tearing down pair`);
    if (!tcp.destroyed) tcp.destroy();
    if (ws.readyState !== ws.CLOSED) ws.terminate();
  };

  ws.on("close", cleanup("WebSocket"));
  ws.on("error", cleanup("WebSocket error"));
  tcp.on("close", cleanup("TCP"));
  tcp.on("error", cleanup("TCP error"));
});
