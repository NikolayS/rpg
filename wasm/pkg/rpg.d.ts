/* tslint:disable */
/* eslint-disable */

/**
 * JavaScript-facing handle for sending input lines into the Rust REPL.
 *
 * Obtain one via [`wasm_line_channel`] and expose it to JS so xterm.js can
 * push user input as the user types.
 */
export class WasmLineSender {
    private constructor();
    free(): void;
    [Symbol.dispose](): void;
    /**
     * Push a line of user input into the REPL.
     *
     * Call this from JavaScript whenever the user presses Enter in the
     * terminal, passing the current input line (without a trailing newline).
     */
    push_line(line: string): void;
    /**
     * Signal EOF (Ctrl-D / terminal closed). The REPL exits cleanly.
     */
    send_eof(): void;
}

/**
 * Start the rpg terminal in the browser.
 *
 * Connects to Postgres via the WebSocket proxy at `ws_url`, then runs the
 * rpg REPL.  Input is read from `window.rpgLineSender` which is set before
 * the REPL loop starts so JS can immediately push lines.
 *
 * # Arguments
 *
 * * `ws_url` — WebSocket URL of the ws-proxy (e.g. `ws://localhost:9091`).
 * * `initial_db` — Optional database name.
 * * `user` — Optional Postgres user; defaults to `"rpg"` if omitted.
 * * `password` — Optional Postgres password; omit for trust-auth connections.
 *
 * # Errors
 *
 * Returns a `JsValue` error if the connection fails or the REPL encounters
 * an unrecoverable error.
 */
export function run_rpg(ws_url: string, initial_db?: string | null, user?: string | null, password?: string | null): Promise<void>;

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
    readonly memory: WebAssembly.Memory;
    readonly __wbg_wasmlinesender_free: (a: number, b: number) => void;
    readonly run_rpg: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number) => number;
    readonly wasmlinesender_push_line: (a: number, b: number, c: number) => void;
    readonly wasmlinesender_send_eof: (a: number) => void;
    readonly __wasm_bindgen_func_elem_2031: (a: number, b: number, c: number, d: number) => void;
    readonly __wasm_bindgen_func_elem_2045: (a: number, b: number, c: number, d: number) => void;
    readonly __wasm_bindgen_func_elem_187: (a: number, b: number, c: number) => void;
    readonly __wasm_bindgen_func_elem_1325: (a: number, b: number, c: number) => void;
    readonly __wasm_bindgen_func_elem_1014: (a: number, b: number) => void;
    readonly __wbindgen_export: (a: number, b: number) => number;
    readonly __wbindgen_export2: (a: number, b: number, c: number, d: number) => number;
    readonly __wbindgen_export3: (a: number) => void;
    readonly __wbindgen_export4: (a: number, b: number, c: number) => void;
    readonly __wbindgen_export5: (a: number, b: number) => void;
    readonly __wbindgen_add_to_stack_pointer: (a: number) => number;
}

export type SyncInitInput = BufferSource | WebAssembly.Module;

/**
 * Instantiates the given `module`, which can either be bytes or
 * a precompiled `WebAssembly.Module`.
 *
 * @param {{ module: SyncInitInput }} module - Passing `SyncInitInput` directly is deprecated.
 *
 * @returns {InitOutput}
 */
export function initSync(module: { module: SyncInitInput } | SyncInitInput): InitOutput;

/**
 * If `module_or_path` is {RequestInfo} or {URL}, makes a request and
 * for everything else, calls `WebAssembly.instantiate` directly.
 *
 * @param {{ module_or_path: InitInput | Promise<InitInput> }} module_or_path - Passing `InitInput` directly is deprecated.
 *
 * @returns {Promise<InitOutput>}
 */
export default function __wbg_init (module_or_path?: { module_or_path: InitInput | Promise<InitInput> } | InitInput | Promise<InitInput>): Promise<InitOutput>;
