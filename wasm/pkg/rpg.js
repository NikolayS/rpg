/**
 * JavaScript-facing handle for sending input lines into the Rust REPL.
 *
 * Obtain one via [`wasm_line_channel`] and expose it to JS so xterm.js can
 * push user input as the user types.
 */
export class WasmLineSender {
    static __wrap(ptr) {
        ptr = ptr >>> 0;
        const obj = Object.create(WasmLineSender.prototype);
        obj.__wbg_ptr = ptr;
        WasmLineSenderFinalization.register(obj, obj.__wbg_ptr, obj);
        return obj;
    }
    __destroy_into_raw() {
        const ptr = this.__wbg_ptr;
        this.__wbg_ptr = 0;
        WasmLineSenderFinalization.unregister(this);
        return ptr;
    }
    free() {
        const ptr = this.__destroy_into_raw();
        wasm.__wbg_wasmlinesender_free(ptr, 0);
    }
    /**
     * Push a line of user input into the REPL.
     *
     * Call this from JavaScript whenever the user presses Enter in the
     * terminal, passing the current input line (without a trailing newline).
     * @param {string} line
     */
    push_line(line) {
        const ptr0 = passStringToWasm0(line, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.wasmlinesender_push_line(this.__wbg_ptr, ptr0, len0);
    }
    /**
     * Signal EOF (Ctrl-D / terminal closed). The REPL exits cleanly.
     */
    send_eof() {
        wasm.wasmlinesender_send_eof(this.__wbg_ptr);
    }
}
if (Symbol.dispose) WasmLineSender.prototype[Symbol.dispose] = WasmLineSender.prototype.free;

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
 * @param {string} ws_url
 * @param {string | null} [initial_db]
 * @param {string | null} [user]
 * @param {string | null} [password]
 * @returns {Promise<void>}
 */
export function run_rpg(ws_url, initial_db, user, password) {
    const ptr0 = passStringToWasm0(ws_url, wasm.__wbindgen_export, wasm.__wbindgen_export2);
    const len0 = WASM_VECTOR_LEN;
    var ptr1 = isLikeNone(initial_db) ? 0 : passStringToWasm0(initial_db, wasm.__wbindgen_export, wasm.__wbindgen_export2);
    var len1 = WASM_VECTOR_LEN;
    var ptr2 = isLikeNone(user) ? 0 : passStringToWasm0(user, wasm.__wbindgen_export, wasm.__wbindgen_export2);
    var len2 = WASM_VECTOR_LEN;
    var ptr3 = isLikeNone(password) ? 0 : passStringToWasm0(password, wasm.__wbindgen_export, wasm.__wbindgen_export2);
    var len3 = WASM_VECTOR_LEN;
    const ret = wasm.run_rpg(ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3);
    return takeObject(ret);
}

function __wbg_get_imports() {
    const import0 = {
        __proto__: null,
        __wbg___wbindgen_debug_string_5398f5bb970e0daa: function(arg0, arg1) {
            const ret = debugString(getObject(arg1));
            const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_export, wasm.__wbindgen_export2);
            const len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbg___wbindgen_is_function_3c846841762788c1: function(arg0) {
            const ret = typeof(getObject(arg0)) === 'function';
            return ret;
        },
        __wbg___wbindgen_is_object_781bc9f159099513: function(arg0) {
            const val = getObject(arg0);
            const ret = typeof(val) === 'object' && val !== null;
            return ret;
        },
        __wbg___wbindgen_is_string_7ef6b97b02428fae: function(arg0) {
            const ret = typeof(getObject(arg0)) === 'string';
            return ret;
        },
        __wbg___wbindgen_is_undefined_52709e72fb9f179c: function(arg0) {
            const ret = getObject(arg0) === undefined;
            return ret;
        },
        __wbg___wbindgen_string_get_395e606bd0ee4427: function(arg0, arg1) {
            const obj = getObject(arg1);
            const ret = typeof(obj) === 'string' ? obj : undefined;
            var ptr1 = isLikeNone(ret) ? 0 : passStringToWasm0(ret, wasm.__wbindgen_export, wasm.__wbindgen_export2);
            var len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbg___wbindgen_throw_6ddd609b62940d55: function(arg0, arg1) {
            throw new Error(getStringFromWasm0(arg0, arg1));
        },
        __wbg__wbg_cb_unref_6b5b6b8576d35cb1: function(arg0) {
            getObject(arg0)._wbg_cb_unref();
        },
        __wbg_abort_5ef96933660780b7: function(arg0) {
            getObject(arg0).abort();
        },
        __wbg_abort_6479c2d794ebf2ee: function(arg0, arg1) {
            getObject(arg0).abort(getObject(arg1));
        },
        __wbg_append_608dfb635ee8998f: function() { return handleError(function (arg0, arg1, arg2, arg3, arg4) {
            getObject(arg0).append(getStringFromWasm0(arg1, arg2), getStringFromWasm0(arg3, arg4));
        }, arguments); },
        __wbg_arrayBuffer_eb8e9ca620af2a19: function() { return handleError(function (arg0) {
            const ret = getObject(arg0).arrayBuffer();
            return addHeapObject(ret);
        }, arguments); },
        __wbg_call_2d781c1f4d5c0ef8: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = getObject(arg0).call(getObject(arg1), getObject(arg2));
            return addHeapObject(ret);
        }, arguments); },
        __wbg_call_e133b57c9155d22c: function() { return handleError(function (arg0, arg1) {
            const ret = getObject(arg0).call(getObject(arg1));
            return addHeapObject(ret);
        }, arguments); },
        __wbg_clearTimeout_6b8d9a38b9263d65: function(arg0) {
            const ret = clearTimeout(takeObject(arg0));
            return addHeapObject(ret);
        },
        __wbg_close_af26905c832a88cb: function() { return handleError(function (arg0) {
            getObject(arg0).close();
        }, arguments); },
        __wbg_code_aea376e2d265a64f: function(arg0) {
            const ret = getObject(arg0).code;
            return ret;
        },
        __wbg_code_bc4dde4d67926010: function(arg0) {
            const ret = getObject(arg0).code;
            return ret;
        },
        __wbg_data_a3d9ff9cdd801002: function(arg0) {
            const ret = getObject(arg0).data;
            return addHeapObject(ret);
        },
        __wbg_done_08ce71ee07e3bd17: function(arg0) {
            const ret = getObject(arg0).done;
            return ret;
        },
        __wbg_error_8d9a8e04cd1d3588: function(arg0) {
            console.error(getObject(arg0));
        },
        __wbg_error_a6fa202b58aa1cd3: function(arg0, arg1) {
            let deferred0_0;
            let deferred0_1;
            try {
                deferred0_0 = arg0;
                deferred0_1 = arg1;
                console.error(getStringFromWasm0(arg0, arg1));
            } finally {
                wasm.__wbindgen_export4(deferred0_0, deferred0_1, 1);
            }
        },
        __wbg_fetch_5550a88cf343aaa9: function(arg0, arg1) {
            const ret = getObject(arg0).fetch(getObject(arg1));
            return addHeapObject(ret);
        },
        __wbg_fetch_9dad4fe911207b37: function(arg0) {
            const ret = fetch(getObject(arg0));
            return addHeapObject(ret);
        },
        __wbg_getRandomValues_3f44b700395062e5: function() { return handleError(function (arg0, arg1) {
            globalThis.crypto.getRandomValues(getArrayU8FromWasm0(arg0, arg1));
        }, arguments); },
        __wbg_get_326e41e095fb2575: function() { return handleError(function (arg0, arg1) {
            const ret = Reflect.get(getObject(arg0), getObject(arg1));
            return addHeapObject(ret);
        }, arguments); },
        __wbg_has_926ef2ff40b308cf: function() { return handleError(function (arg0, arg1) {
            const ret = Reflect.has(getObject(arg0), getObject(arg1));
            return ret;
        }, arguments); },
        __wbg_headers_eb2234545f9ff993: function(arg0) {
            const ret = getObject(arg0).headers;
            return addHeapObject(ret);
        },
        __wbg_instanceof_ArrayBuffer_101e2bf31071a9f6: function(arg0) {
            let result;
            try {
                result = getObject(arg0) instanceof ArrayBuffer;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_instanceof_Blob_c91af000f11c2d0b: function(arg0) {
            let result;
            try {
                result = getObject(arg0) instanceof Blob;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_instanceof_Response_9b4d9fd451e051b1: function(arg0) {
            let result;
            try {
                result = getObject(arg0) instanceof Response;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_iterator_d8f549ec8fb061b1: function() {
            const ret = Symbol.iterator;
            return addHeapObject(ret);
        },
        __wbg_length_ea16607d7b61445b: function(arg0) {
            const ret = getObject(arg0).length;
            return ret;
        },
        __wbg_log_524eedafa26daa59: function(arg0) {
            console.log(getObject(arg0));
        },
        __wbg_message_e959edc81e4b6cb7: function(arg0, arg1) {
            const ret = getObject(arg1).message;
            const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_export, wasm.__wbindgen_export2);
            const len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbg_new_0837727332ac86ba: function() { return handleError(function () {
            const ret = new Headers();
            return addHeapObject(ret);
        }, arguments); },
        __wbg_new_227d7c05414eb861: function() {
            const ret = new Error();
            return addHeapObject(ret);
        },
        __wbg_new_5f486cdf45a04d78: function(arg0) {
            const ret = new Uint8Array(getObject(arg0));
            return addHeapObject(ret);
        },
        __wbg_new_a70fbab9066b301f: function() {
            const ret = new Array();
            return addHeapObject(ret);
        },
        __wbg_new_ab79df5bd7c26067: function() {
            const ret = new Object();
            return addHeapObject(ret);
        },
        __wbg_new_c518c60af666645b: function() { return handleError(function () {
            const ret = new AbortController();
            return addHeapObject(ret);
        }, arguments); },
        __wbg_new_dd50bcc3f60ba434: function() { return handleError(function (arg0, arg1) {
            const ret = new WebSocket(getStringFromWasm0(arg0, arg1));
            return addHeapObject(ret);
        }, arguments); },
        __wbg_new_from_slice_22da9388ac046e50: function(arg0, arg1) {
            const ret = new Uint8Array(getArrayU8FromWasm0(arg0, arg1));
            return addHeapObject(ret);
        },
        __wbg_new_typed_aaaeaf29cf802876: function(arg0, arg1) {
            try {
                var state0 = {a: arg0, b: arg1};
                var cb0 = (arg0, arg1) => {
                    const a = state0.a;
                    state0.a = 0;
                    try {
                        return __wasm_bindgen_func_elem_1588(a, state0.b, arg0, arg1);
                    } finally {
                        state0.a = a;
                    }
                };
                const ret = new Promise(cb0);
                return addHeapObject(ret);
            } finally {
                state0.a = state0.b = 0;
            }
        },
        __wbg_new_with_str_and_init_b4b54d1a819bc724: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = new Request(getStringFromWasm0(arg0, arg1), getObject(arg2));
            return addHeapObject(ret);
        }, arguments); },
        __wbg_new_with_str_sequence_82c04ad794ead10e: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = new WebSocket(getStringFromWasm0(arg0, arg1), getObject(arg2));
            return addHeapObject(ret);
        }, arguments); },
        __wbg_next_11b99ee6237339e3: function() { return handleError(function (arg0) {
            const ret = getObject(arg0).next();
            return addHeapObject(ret);
        }, arguments); },
        __wbg_next_e01a967809d1aa68: function(arg0) {
            const ret = getObject(arg0).next;
            return addHeapObject(ret);
        },
        __wbg_prototypesetcall_d62e5099504357e6: function(arg0, arg1, arg2) {
            Uint8Array.prototype.set.call(getArrayU8FromWasm0(arg0, arg1), getObject(arg2));
        },
        __wbg_push_e87b0e732085a946: function(arg0, arg1) {
            const ret = getObject(arg0).push(getObject(arg1));
            return ret;
        },
        __wbg_queueMicrotask_0c399741342fb10f: function(arg0) {
            const ret = getObject(arg0).queueMicrotask;
            return addHeapObject(ret);
        },
        __wbg_queueMicrotask_a082d78ce798393e: function(arg0) {
            queueMicrotask(getObject(arg0));
        },
        __wbg_readyState_1f1e7f1bdf9f4d42: function(arg0) {
            const ret = getObject(arg0).readyState;
            return ret;
        },
        __wbg_reason_cbcb9911796c4714: function(arg0, arg1) {
            const ret = getObject(arg1).reason;
            const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_export, wasm.__wbindgen_export2);
            const len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbg_resolve_ae8d83246e5bcc12: function(arg0) {
            const ret = Promise.resolve(getObject(arg0));
            return addHeapObject(ret);
        },
        __wbg_send_4a1dc66e8653e5ed: function() { return handleError(function (arg0, arg1, arg2) {
            getObject(arg0).send(getStringFromWasm0(arg1, arg2));
        }, arguments); },
        __wbg_send_d31a693c975dea74: function() { return handleError(function (arg0, arg1, arg2) {
            getObject(arg0).send(getArrayU8FromWasm0(arg1, arg2));
        }, arguments); },
        __wbg_setTimeout_f757f00851f76c42: function(arg0, arg1) {
            const ret = setTimeout(getObject(arg0), arg1);
            return addHeapObject(ret);
        },
        __wbg_set_7eaa4f96924fd6b3: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = Reflect.set(getObject(arg0), getObject(arg1), getObject(arg2));
            return ret;
        }, arguments); },
        __wbg_set_binaryType_3dcf8281ec100a8f: function(arg0, arg1) {
            getObject(arg0).binaryType = __wbindgen_enum_BinaryType[arg1];
        },
        __wbg_set_body_a3d856b097dfda04: function(arg0, arg1) {
            getObject(arg0).body = getObject(arg1);
        },
        __wbg_set_cache_ec7e430c6056ebda: function(arg0, arg1) {
            getObject(arg0).cache = __wbindgen_enum_RequestCache[arg1];
        },
        __wbg_set_credentials_ed63183445882c65: function(arg0, arg1) {
            getObject(arg0).credentials = __wbindgen_enum_RequestCredentials[arg1];
        },
        __wbg_set_headers_3c8fecc693b75327: function(arg0, arg1) {
            getObject(arg0).headers = getObject(arg1);
        },
        __wbg_set_method_8c015e8bcafd7be1: function(arg0, arg1, arg2) {
            getObject(arg0).method = getStringFromWasm0(arg1, arg2);
        },
        __wbg_set_mode_5a87f2c809cf37c2: function(arg0, arg1) {
            getObject(arg0).mode = __wbindgen_enum_RequestMode[arg1];
        },
        __wbg_set_onclose_8da801226bdd7a7b: function(arg0, arg1) {
            getObject(arg0).onclose = getObject(arg1);
        },
        __wbg_set_onerror_901ca711f94a5bbb: function(arg0, arg1) {
            getObject(arg0).onerror = getObject(arg1);
        },
        __wbg_set_onmessage_6f80ab771bf151aa: function(arg0, arg1) {
            getObject(arg0).onmessage = getObject(arg1);
        },
        __wbg_set_onopen_34e3e24cf9337ddd: function(arg0, arg1) {
            getObject(arg0).onopen = getObject(arg1);
        },
        __wbg_set_signal_0cebecb698f25d21: function(arg0, arg1) {
            getObject(arg0).signal = getObject(arg1);
        },
        __wbg_signal_166e1da31adcac18: function(arg0) {
            const ret = getObject(arg0).signal;
            return addHeapObject(ret);
        },
        __wbg_stack_3b0d974bbf31e44f: function(arg0, arg1) {
            const ret = getObject(arg1).stack;
            const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_export, wasm.__wbindgen_export2);
            const len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbg_static_accessor_GLOBAL_8adb955bd33fac2f: function() {
            const ret = typeof global === 'undefined' ? null : global;
            return isLikeNone(ret) ? 0 : addHeapObject(ret);
        },
        __wbg_static_accessor_GLOBAL_THIS_ad356e0db91c7913: function() {
            const ret = typeof globalThis === 'undefined' ? null : globalThis;
            return isLikeNone(ret) ? 0 : addHeapObject(ret);
        },
        __wbg_static_accessor_SELF_f207c857566db248: function() {
            const ret = typeof self === 'undefined' ? null : self;
            return isLikeNone(ret) ? 0 : addHeapObject(ret);
        },
        __wbg_static_accessor_WINDOW_bb9f1ba69d61b386: function() {
            const ret = typeof window === 'undefined' ? null : window;
            return isLikeNone(ret) ? 0 : addHeapObject(ret);
        },
        __wbg_status_318629ab93a22955: function(arg0) {
            const ret = getObject(arg0).status;
            return ret;
        },
        __wbg_stringify_5ae93966a84901ac: function() { return handleError(function (arg0) {
            const ret = JSON.stringify(getObject(arg0));
            return addHeapObject(ret);
        }, arguments); },
        __wbg_text_372f5b91442c50f9: function() { return handleError(function (arg0) {
            const ret = getObject(arg0).text();
            return addHeapObject(ret);
        }, arguments); },
        __wbg_then_098abe61755d12f6: function(arg0, arg1) {
            const ret = getObject(arg0).then(getObject(arg1));
            return addHeapObject(ret);
        },
        __wbg_then_9e335f6dd892bc11: function(arg0, arg1, arg2) {
            const ret = getObject(arg0).then(getObject(arg1), getObject(arg2));
            return addHeapObject(ret);
        },
        __wbg_url_7fefc1820fba4e0c: function(arg0, arg1) {
            const ret = getObject(arg1).url;
            const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_export, wasm.__wbindgen_export2);
            const len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbg_value_21fc78aab0322612: function(arg0) {
            const ret = getObject(arg0).value;
            return addHeapObject(ret);
        },
        __wbg_wasClean_69f68dc4ed2d2cc7: function(arg0) {
            const ret = getObject(arg0).wasClean;
            return ret;
        },
        __wbg_wasmlinesender_new: function(arg0) {
            const ret = WasmLineSender.__wrap(arg0);
            return addHeapObject(ret);
        },
        __wbindgen_cast_0000000000000001: function(arg0, arg1) {
            // Cast intrinsic for `Closure(Closure { dtor_idx: 15, function: Function { arguments: [Externref], shim_idx: 16, ret: Result(Unit), inner_ret: Some(Result(Unit)) }, mutable: true }) -> Externref`.
            const ret = makeMutClosure(arg0, arg1, wasm.__wasm_bindgen_func_elem_217, __wasm_bindgen_func_elem_1606);
            return addHeapObject(ret);
        },
        __wbindgen_cast_0000000000000002: function(arg0, arg1) {
            // Cast intrinsic for `Closure(Closure { dtor_idx: 15, function: Function { arguments: [NamedExternref("CloseEvent")], shim_idx: 20, ret: Unit, inner_ret: Some(Unit) }, mutable: true }) -> Externref`.
            const ret = makeMutClosure(arg0, arg1, wasm.__wasm_bindgen_func_elem_217, __wasm_bindgen_func_elem_218);
            return addHeapObject(ret);
        },
        __wbindgen_cast_0000000000000003: function(arg0, arg1) {
            // Cast intrinsic for `Closure(Closure { dtor_idx: 15, function: Function { arguments: [NamedExternref("MessageEvent")], shim_idx: 20, ret: Unit, inner_ret: Some(Unit) }, mutable: true }) -> Externref`.
            const ret = makeMutClosure(arg0, arg1, wasm.__wasm_bindgen_func_elem_217, __wasm_bindgen_func_elem_218_2);
            return addHeapObject(ret);
        },
        __wbindgen_cast_0000000000000004: function(arg0, arg1) {
            // Cast intrinsic for `Closure(Closure { dtor_idx: 15, function: Function { arguments: [], shim_idx: 52, ret: Unit, inner_ret: Some(Unit) }, mutable: true }) -> Externref`.
            const ret = makeMutClosure(arg0, arg1, wasm.__wasm_bindgen_func_elem_217, __wasm_bindgen_func_elem_2233);
            return addHeapObject(ret);
        },
        __wbindgen_cast_0000000000000005: function(arg0, arg1) {
            // Cast intrinsic for `Ref(String) -> Externref`.
            const ret = getStringFromWasm0(arg0, arg1);
            return addHeapObject(ret);
        },
        __wbindgen_object_clone_ref: function(arg0) {
            const ret = getObject(arg0);
            return addHeapObject(ret);
        },
        __wbindgen_object_drop_ref: function(arg0) {
            takeObject(arg0);
        },
    };
    return {
        __proto__: null,
        "./rpg_bg.js": import0,
    };
}

function __wasm_bindgen_func_elem_2233(arg0, arg1) {
    wasm.__wasm_bindgen_func_elem_2233(arg0, arg1);
}

function __wasm_bindgen_func_elem_218(arg0, arg1, arg2) {
    wasm.__wasm_bindgen_func_elem_218(arg0, arg1, addHeapObject(arg2));
}

function __wasm_bindgen_func_elem_218_2(arg0, arg1, arg2) {
    wasm.__wasm_bindgen_func_elem_218_2(arg0, arg1, addHeapObject(arg2));
}

function __wasm_bindgen_func_elem_1606(arg0, arg1, arg2) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.__wasm_bindgen_func_elem_1606(retptr, arg0, arg1, addHeapObject(arg2));
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        if (r1) {
            throw takeObject(r0);
        }
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

function __wasm_bindgen_func_elem_1588(arg0, arg1, arg2, arg3) {
    wasm.__wasm_bindgen_func_elem_1588(arg0, arg1, addHeapObject(arg2), addHeapObject(arg3));
}


const __wbindgen_enum_BinaryType = ["blob", "arraybuffer"];


const __wbindgen_enum_RequestCache = ["default", "no-store", "reload", "no-cache", "force-cache", "only-if-cached"];


const __wbindgen_enum_RequestCredentials = ["omit", "same-origin", "include"];


const __wbindgen_enum_RequestMode = ["same-origin", "no-cors", "cors", "navigate"];
const WasmLineSenderFinalization = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(ptr => wasm.__wbg_wasmlinesender_free(ptr >>> 0, 1));

function addHeapObject(obj) {
    if (heap_next === heap.length) heap.push(heap.length + 1);
    const idx = heap_next;
    heap_next = heap[idx];

    heap[idx] = obj;
    return idx;
}

const CLOSURE_DTORS = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(state => state.dtor(state.a, state.b));

function debugString(val) {
    // primitive types
    const type = typeof val;
    if (type == 'number' || type == 'boolean' || val == null) {
        return  `${val}`;
    }
    if (type == 'string') {
        return `"${val}"`;
    }
    if (type == 'symbol') {
        const description = val.description;
        if (description == null) {
            return 'Symbol';
        } else {
            return `Symbol(${description})`;
        }
    }
    if (type == 'function') {
        const name = val.name;
        if (typeof name == 'string' && name.length > 0) {
            return `Function(${name})`;
        } else {
            return 'Function';
        }
    }
    // objects
    if (Array.isArray(val)) {
        const length = val.length;
        let debug = '[';
        if (length > 0) {
            debug += debugString(val[0]);
        }
        for(let i = 1; i < length; i++) {
            debug += ', ' + debugString(val[i]);
        }
        debug += ']';
        return debug;
    }
    // Test for built-in
    const builtInMatches = /\[object ([^\]]+)\]/.exec(toString.call(val));
    let className;
    if (builtInMatches && builtInMatches.length > 1) {
        className = builtInMatches[1];
    } else {
        // Failed to match the standard '[object ClassName]'
        return toString.call(val);
    }
    if (className == 'Object') {
        // we're a user defined class or Object
        // JSON.stringify avoids problems with cycles, and is generally much
        // easier than looping through ownProperties of `val`.
        try {
            return 'Object(' + JSON.stringify(val) + ')';
        } catch (_) {
            return 'Object';
        }
    }
    // errors
    if (val instanceof Error) {
        return `${val.name}: ${val.message}\n${val.stack}`;
    }
    // TODO we could test for more things here, like `Set`s and `Map`s.
    return className;
}

function dropObject(idx) {
    if (idx < 1028) return;
    heap[idx] = heap_next;
    heap_next = idx;
}

function getArrayU8FromWasm0(ptr, len) {
    ptr = ptr >>> 0;
    return getUint8ArrayMemory0().subarray(ptr / 1, ptr / 1 + len);
}

let cachedDataViewMemory0 = null;
function getDataViewMemory0() {
    if (cachedDataViewMemory0 === null || cachedDataViewMemory0.buffer.detached === true || (cachedDataViewMemory0.buffer.detached === undefined && cachedDataViewMemory0.buffer !== wasm.memory.buffer)) {
        cachedDataViewMemory0 = new DataView(wasm.memory.buffer);
    }
    return cachedDataViewMemory0;
}

function getStringFromWasm0(ptr, len) {
    ptr = ptr >>> 0;
    return decodeText(ptr, len);
}

let cachedUint8ArrayMemory0 = null;
function getUint8ArrayMemory0() {
    if (cachedUint8ArrayMemory0 === null || cachedUint8ArrayMemory0.byteLength === 0) {
        cachedUint8ArrayMemory0 = new Uint8Array(wasm.memory.buffer);
    }
    return cachedUint8ArrayMemory0;
}

function getObject(idx) { return heap[idx]; }

function handleError(f, args) {
    try {
        return f.apply(this, args);
    } catch (e) {
        wasm.__wbindgen_export3(addHeapObject(e));
    }
}

let heap = new Array(1024).fill(undefined);
heap.push(undefined, null, true, false);

let heap_next = heap.length;

function isLikeNone(x) {
    return x === undefined || x === null;
}

function makeMutClosure(arg0, arg1, dtor, f) {
    const state = { a: arg0, b: arg1, cnt: 1, dtor };
    const real = (...args) => {

        // First up with a closure we increment the internal reference
        // count. This ensures that the Rust closure environment won't
        // be deallocated while we're invoking it.
        state.cnt++;
        const a = state.a;
        state.a = 0;
        try {
            return f(a, state.b, ...args);
        } finally {
            state.a = a;
            real._wbg_cb_unref();
        }
    };
    real._wbg_cb_unref = () => {
        if (--state.cnt === 0) {
            state.dtor(state.a, state.b);
            state.a = 0;
            CLOSURE_DTORS.unregister(state);
        }
    };
    CLOSURE_DTORS.register(real, state, state);
    return real;
}

function passStringToWasm0(arg, malloc, realloc) {
    if (realloc === undefined) {
        const buf = cachedTextEncoder.encode(arg);
        const ptr = malloc(buf.length, 1) >>> 0;
        getUint8ArrayMemory0().subarray(ptr, ptr + buf.length).set(buf);
        WASM_VECTOR_LEN = buf.length;
        return ptr;
    }

    let len = arg.length;
    let ptr = malloc(len, 1) >>> 0;

    const mem = getUint8ArrayMemory0();

    let offset = 0;

    for (; offset < len; offset++) {
        const code = arg.charCodeAt(offset);
        if (code > 0x7F) break;
        mem[ptr + offset] = code;
    }
    if (offset !== len) {
        if (offset !== 0) {
            arg = arg.slice(offset);
        }
        ptr = realloc(ptr, len, len = offset + arg.length * 3, 1) >>> 0;
        const view = getUint8ArrayMemory0().subarray(ptr + offset, ptr + len);
        const ret = cachedTextEncoder.encodeInto(arg, view);

        offset += ret.written;
        ptr = realloc(ptr, len, offset, 1) >>> 0;
    }

    WASM_VECTOR_LEN = offset;
    return ptr;
}

function takeObject(idx) {
    const ret = getObject(idx);
    dropObject(idx);
    return ret;
}

let cachedTextDecoder = new TextDecoder('utf-8', { ignoreBOM: true, fatal: true });
cachedTextDecoder.decode();
const MAX_SAFARI_DECODE_BYTES = 2146435072;
let numBytesDecoded = 0;
function decodeText(ptr, len) {
    numBytesDecoded += len;
    if (numBytesDecoded >= MAX_SAFARI_DECODE_BYTES) {
        cachedTextDecoder = new TextDecoder('utf-8', { ignoreBOM: true, fatal: true });
        cachedTextDecoder.decode();
        numBytesDecoded = len;
    }
    return cachedTextDecoder.decode(getUint8ArrayMemory0().subarray(ptr, ptr + len));
}

const cachedTextEncoder = new TextEncoder();

if (!('encodeInto' in cachedTextEncoder)) {
    cachedTextEncoder.encodeInto = function (arg, view) {
        const buf = cachedTextEncoder.encode(arg);
        view.set(buf);
        return {
            read: arg.length,
            written: buf.length
        };
    };
}

let WASM_VECTOR_LEN = 0;

let wasmModule, wasm;
function __wbg_finalize_init(instance, module) {
    wasm = instance.exports;
    wasmModule = module;
    cachedDataViewMemory0 = null;
    cachedUint8ArrayMemory0 = null;
    wasm.__wbindgen_start();
    return wasm;
}

async function __wbg_load(module, imports) {
    if (typeof Response === 'function' && module instanceof Response) {
        if (typeof WebAssembly.instantiateStreaming === 'function') {
            try {
                return await WebAssembly.instantiateStreaming(module, imports);
            } catch (e) {
                const validResponse = module.ok && expectedResponseType(module.type);

                if (validResponse && module.headers.get('Content-Type') !== 'application/wasm') {
                    console.warn("`WebAssembly.instantiateStreaming` failed because your server does not serve Wasm with `application/wasm` MIME type. Falling back to `WebAssembly.instantiate` which is slower. Original error:\n", e);

                } else { throw e; }
            }
        }

        const bytes = await module.arrayBuffer();
        return await WebAssembly.instantiate(bytes, imports);
    } else {
        const instance = await WebAssembly.instantiate(module, imports);

        if (instance instanceof WebAssembly.Instance) {
            return { instance, module };
        } else {
            return instance;
        }
    }

    function expectedResponseType(type) {
        switch (type) {
            case 'basic': case 'cors': case 'default': return true;
        }
        return false;
    }
}

function initSync(module) {
    if (wasm !== undefined) return wasm;


    if (module !== undefined) {
        if (Object.getPrototypeOf(module) === Object.prototype) {
            ({module} = module)
        } else {
            console.warn('using deprecated parameters for `initSync()`; pass a single object instead')
        }
    }

    const imports = __wbg_get_imports();
    if (!(module instanceof WebAssembly.Module)) {
        module = new WebAssembly.Module(module);
    }
    const instance = new WebAssembly.Instance(module, imports);
    return __wbg_finalize_init(instance, module);
}

async function __wbg_init(module_or_path) {
    if (wasm !== undefined) return wasm;


    if (module_or_path !== undefined) {
        if (Object.getPrototypeOf(module_or_path) === Object.prototype) {
            ({module_or_path} = module_or_path)
        } else {
            console.warn('using deprecated parameters for the initialization function; pass a single object instead')
        }
    }

    if (module_or_path === undefined) {
        module_or_path = new URL('rpg_bg.wasm', import.meta.url);
    }
    const imports = __wbg_get_imports();

    if (typeof module_or_path === 'string' || (typeof Request === 'function' && module_or_path instanceof Request) || (typeof URL === 'function' && module_or_path instanceof URL)) {
        module_or_path = fetch(module_or_path);
    }

    const { instance, module } = await __wbg_load(await module_or_path, imports);

    return __wbg_finalize_init(instance, module);
}

export { initSync, __wbg_init as default };
