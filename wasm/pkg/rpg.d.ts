/* tslint:disable */
/* eslint-disable */

/**
 * Start the rpg terminal in the browser.
 *
 * # Arguments
 *
 * * `ws_url` — WebSocket URL of the ws-proxy (e.g. `ws://localhost:9091`).
 * * `initial_db` — Optional database name; overrides the connection string
 *   default if provided.
 * * `user` — Optional Postgres user; defaults to `"rpg"` if not provided.
 *
 * # Errors
 *
 * Returns a `JsValue` error if the connection fails or the REPL encounters
 * an unrecoverable error.
 */
export function run_rpg(ws_url: string, initial_db?: string | null, user?: string | null): Promise<void>;

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
    readonly memory: WebAssembly.Memory;
    readonly run_rpg: (a: number, b: number, c: number, d: number, e: number, f: number) => number;
    readonly main: (a: number, b: number) => number;
    readonly __wasm_bindgen_func_elem_74: (a: number, b: number) => void;
    readonly __wasm_bindgen_func_elem_667: (a: number, b: number, c: number, d: number) => void;
    readonly __wasm_bindgen_func_elem_649: (a: number, b: number, c: number, d: number) => void;
    readonly __wasm_bindgen_func_elem_75: (a: number, b: number, c: number) => void;
    readonly __wasm_bindgen_func_elem_75_2: (a: number, b: number, c: number) => void;
    readonly __wasm_bindgen_func_elem_1292: (a: number, b: number) => void;
    readonly __wbindgen_export: (a: number, b: number) => number;
    readonly __wbindgen_export2: (a: number, b: number, c: number, d: number) => number;
    readonly __wbindgen_export3: (a: number) => void;
    readonly __wbindgen_export4: (a: number, b: number, c: number) => void;
    readonly __wbindgen_add_to_stack_pointer: (a: number) => number;
    readonly __wbindgen_start: () => void;
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
