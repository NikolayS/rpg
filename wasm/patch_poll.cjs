#!/usr/bin/env node
// Patch script to make poll() and connect() async-aware using Asyncify

const fs = require('fs');
const path = process.argv[2] || 'wasm-build/psql.js';

let code = fs.readFileSync(path, 'utf8');
let patched = false;

// Patch 1: __poll_js function
const newPoll = `function __poll_js(fds, nfds, timeout, ctx, arg) {
  // Async-aware poll implementation using Asyncify

  // During rewind, just call handleSleep to get the return value
  if (Asyncify.state === Asyncify.State.Rewinding) {
    return Asyncify.handleSleep(function(wakeUp) {});
  }

  function doPoll() {
    var count = 0;
    for (var i = 0; i < nfds; i++) {
      var pollfd = fds + 8 * i;
      var fd = HEAP32[((pollfd)>>2)];
      var events = HEAP16[(((pollfd)+(4))>>1)];
      var flags = 32;
      var stream = FS.getStream(fd);
      if (stream) {
        if (stream.stream_ops.poll) {
          flags = stream.stream_ops.poll(stream, -1);
        } else {
          flags = 5;
        }
      }
      flags &= events | 8 | 16;
      if (flags) count++;
      HEAP16[(((pollfd)+(6))>>1)] = flags;
    }
    return count;
  }

  try {
    var count = doPoll();

    // If no data and timeout is non-zero, use Asyncify to wait
    if (!count && timeout != 0) {
      runtimeKeepaliveCounter++;  // Prevent exit during async operation
      return Asyncify.handleSleep(function(wakeUp) {
        var startTime = Date.now();
        var maxWait = timeout < 0 ? 30000 : Math.min(timeout, 30000);

        function checkAgain() {
          var result = doPoll();
          if (result > 0) {
            runtimeKeepaliveCounter--;  // Allow exit again
            wakeUp(result);
            return;
          }

          var elapsed = Date.now() - startTime;
          if (elapsed >= maxWait) {
            runtimeKeepaliveCounter--;  // Allow exit again
            wakeUp(0); // Timeout
            return;
          }

          // Check again in 50ms
          setTimeout(checkAgain, 50);
        }

        // Start checking after a short delay to let WebSocket events fire
        setTimeout(checkAgain, 10);
      });
    }

    return count;
  } catch (e) {
    if (typeof FS == 'undefined' || !(e.name === 'ErrnoError')) throw e;
    return -e.errno;
  }
}`;

// Match both -O2 (with warnOnce) and -Oz (without warnOnce) outputs
const pollRegex = /function __poll_js\(fds, nfds, timeout, ctx, arg\) \{\s*try \{[\s\S]*?return -e\.errno;\s*\}\s*\}/;

if (pollRegex.test(code)) {
  code = code.replace(pollRegex, newPoll);
  console.log('Patched __poll_js function');
  patched = true;
} else {
  console.error('Warning: Could not find __poll_js function to patch');
}

// Patch 2: ___syscall_connect function
const newConnect = `function ___syscall_connect(fd, addr, addrlen, d1, d2, d3) {
try {
    // During rewind, just call handleSleep to get the return value
    if (Asyncify.state === Asyncify.State.Rewinding) {
      return Asyncify.handleSleep(function(wakeUp) {});
    }

    var sock = getSocketFromFD(fd);
    var info = getSocketAddress(addr, addrlen);
    sock.sock_ops.connect(sock, info.addr, info.port);

    // Wait for WebSocket to actually open before returning
    var dest = SOCKFS.websocket_sock_ops.getPeer(sock, sock.daddr, sock.dport);
    if (dest && dest.socket.readyState === dest.socket.CONNECTING) {
      runtimeKeepaliveCounter++;  // Prevent exit during async operation
      return Asyncify.handleSleep(function(wakeUp) {
        function checkOpen() {
          if (dest.socket.readyState === dest.socket.OPEN) {
            sock.connecting = false;
            runtimeKeepaliveCounter--;  // Allow exit again
            wakeUp(0);  // Success
          } else if (dest.socket.readyState === dest.socket.CLOSED ||
                     dest.socket.readyState === dest.socket.CLOSING) {
            runtimeKeepaliveCounter--;  // Allow exit again
            wakeUp(-111);  // ECONNREFUSED
          } else {
            setTimeout(checkOpen, 50);
          }
        }
        setTimeout(checkOpen, 10);
      });
    }

    return 0;
  } catch (e) {
  if (typeof FS == 'undefined' || !(e.name === 'ErrnoError')) throw e;
  return -e.errno;
}
}`;

const connectRegex = /function ___syscall_connect\(fd, addr, addrlen, d1, d2, d3\) \{\s*try \{[\s\S]*?sock\.sock_ops\.connect\(sock, info\.addr, info\.port\);\s*return 0;[\s\S]*?return -e\.errno;\s*\}\s*\}/;

if (connectRegex.test(code)) {
  code = code.replace(connectRegex, newConnect);
  console.log('Patched ___syscall_connect function');
  patched = true;
} else {
  console.error('Warning: Could not find ___syscall_connect function to patch');
}

// Patch 3: exitJS function - skip exit during Asyncify async operation
// Match both -O2 (checkUnflushedContent) and -Oz (_proc_exit) outputs
const exitJsOld = /var exitJS = \(status, implicit\) => \{\s*EXITSTATUS = status;\s*(?:checkUnflushedContent\(\);|_proc_exit\(status\);)/;
const exitJsNew = `var exitJS = (status, implicit) => {
    // Skip exit during Asyncify async operation (callMain returns on first suspend)
    if (Asyncify.currData) {
      return;
    }
    EXITSTATUS = status;
    _proc_exit(status);`;

if (exitJsOld.test(code)) {
  code = code.replace(exitJsOld, exitJsNew);
  console.log('Patched exitJS function');
  patched = true;
} else {
  console.error('Warning: Could not find exitJS function to patch');
}

// Patch 4: _fd_read function - async stdin support
const newFdRead = `function _fd_read(fd, iov, iovcnt, pnum) {
  // Async-aware fd_read for stdin using Asyncify

  try {
      var stream = SYSCALLS.getStreamFromFD(fd);

      // For stdin (fd 0), handle async input
      if (fd === 0 && Module['psqlInput']) {

        // During rewind, read the input that's now available
        if (Asyncify.state === Asyncify.State.Rewinding) {
          Asyncify.handleSleep(function(wakeUp) {});

          var totalRead = 0;
          var iovCopy = iov;
          for (var i = 0; i < iovcnt; i++) {
            var ptr = HEAPU32[((iovCopy)>>2)];
            var len = HEAPU32[(((iovCopy)+(4))>>2)];
            iovCopy += 8;
            for (var j = 0; j < len; j++) {
              var char = Module['psqlInput'].getChar();
              if (char === null) break;
              HEAP8[ptr + j] = char;
              totalRead++;
            }
            if (Module['psqlInput'].buffer.length === 0) break;
          }
          HEAPU32[((pnum)>>2)] = totalRead;
          return 0;
        }

        // Check if input is available
        var char = Module['psqlInput'].getChar();
        if (char !== null) {
          Module['psqlInput'].buffer.unshift(char);
          var totalRead = 0;
          for (var i = 0; i < iovcnt; i++) {
            var ptr = HEAPU32[((iov)>>2)];
            var len = HEAPU32[(((iov)+(4))>>2)];
            iov += 8;
            for (var j = 0; j < len; j++) {
              var c = Module['psqlInput'].getChar();
              if (c === null) break;
              HEAP8[ptr + j] = c;
              totalRead++;
            }
            if (Module['psqlInput'].buffer.length === 0) break;
          }
          HEAPU32[((pnum)>>2)] = totalRead;
          return 0;
        }

        // No input - flush stdout TTY buffer (shows the prompt) and wait asynchronously
        var stdoutStream = FS.getStream(1);
        if (stdoutStream && stdoutStream.tty && stdoutStream.tty.output && stdoutStream.tty.output.length > 0) {
          var promptText = String.fromCharCode.apply(null, stdoutStream.tty.output);
          if (Module['psqlTerminal']) Module['psqlTerminal'].write(promptText);
          stdoutStream.tty.output = [];
        }
        runtimeKeepaliveCounter++;
        return Asyncify.handleSleep(function(wakeUp) {
          Module['psqlInput'].waitForInput().then(function() {
            runtimeKeepaliveCounter--;
            wakeUp(0);
          });
        });
      }

      // Non-stdin: handle rewind
      if (Asyncify.state === Asyncify.State.Rewinding) {
        return Asyncify.handleSleep(function(wakeUp) {});
      }

      // Non-stdin: synchronous read
      var num = doReadv(stream, iov, iovcnt);
      HEAPU32[((pnum)>>2)] = num;
      return 0;
    } catch (e) {
    if (typeof FS == 'undefined' || !(e.name === 'ErrnoError')) throw e;
    return e.errno;
  }
  }`;

// Match both quote styles and whitespace variants (>> vs >> with spaces)
const fdReadRegex = /function _fd_read\(fd, iov, iovcnt, pnum\) \{\s*try \{\s*var stream = SYSCALLS\.getStreamFromFD\(fd\);\s*var num = doReadv\(stream, iov, iovcnt\);\s*HEAPU32\[\(\(pnum\)\s*>>\s*2\)\]\s*=\s*num;\s*return 0;\s*\} catch \(e\) \{\s*if \(typeof FS ==\s*['"]undefined['"]\s*\|\|\s*!\(e\.name ===\s*['"]ErrnoError['"]\)\) throw e;\s*return e\.errno;\s*\}\s*\}/;

if (fdReadRegex.test(code)) {
  code = code.replace(fdReadRegex, newFdRead);
  console.log('Patched _fd_read function');
  patched = true;
} else {
  console.error('Warning: Could not find _fd_read function to patch');
}

// Patch 5: importPattern - add WASI fd_read to async imports
// Match any ordering of the names in the regex pattern
const importPatternOld = /var importPattern = \/\^\([\w|_.*]+\)\$\/;/;
const importPatternNew = `var importPattern = /^(_poll_js|__poll_js|__syscall_connect|___syscall_connect|fd_read|_fd_read|__wasi_fd_read|invoke_.*|__asyncjs__.*)$/;`;

if (importPatternOld.test(code)) {
  code = code.replace(importPatternOld, importPatternNew);
  console.log('Patched importPattern');
  patched = true;
} else {
  console.error('Warning: Could not find importPattern to patch');
}


// Patch 6: _proc_exit - always call Module.onExit before quitting
// psql's C exit() calls _proc_exit directly via WASM import, bypassing exitJS.
// The default implementation only calls Module.onExit when !keepRuntimeAlive(),
// but our async patches keep runtimeKeepaliveCounter > 0, so onExit never fires.
const procExitOld = /var _proc_exit = code => \{\s*EXITSTATUS = code;\s*if \(!keepRuntimeAlive\(\)\) \{\s*Module\["onExit"\]\?\.\(code\);\s*ABORT = true;\s*\}\s*quit_\(code, new ExitStatus\(code\)\);\s*\};/;
const procExitNew = `var _proc_exit = code => {
  EXITSTATUS = code;
  Module["onExit"]?.(code);
  if (!keepRuntimeAlive()) {
    ABORT = true;
  }
  quit_(code, new ExitStatus(code));
};`;

if (procExitOld.test(code)) {
  code = code.replace(procExitOld, procExitNew);
  console.log('Patched _proc_exit function');
  patched = true;
} else {
  console.error('Warning: Could not find _proc_exit function to patch');
}

// Patch 7: Asyncify doRewind completion - fire Module.onExit when WASM program finishes
// With EXIT_RUNTIME=0, psql's exit() is a no-op that never calls _exit or _proc_exit.
// When doRewind completes (currData becomes null), the WASM program has finished.
// We detect this and fire Module.onExit so the host can clean up (e.g. close terminal tab).
const doRewindOld = /try \{\s*asyncWasmReturnValue = Asyncify\.doRewind\(Asyncify\.currData\);\s*\} catch \(err\) \{\s*asyncWasmReturnValue = err;\s*isError = true;\s*\}\s*\/\/ Track whether the return value was handled/;
const doRewindNew = `try {
          asyncWasmReturnValue = Asyncify.doRewind(Asyncify.currData);
        } catch (err) {
          asyncWasmReturnValue = err;
          isError = true;
        }
        // When currData is null and doRewind returned normally, the WASM
        // program has finished (e.g. psql \\q).  Fire onExit since
        // EXIT_RUNTIME=0 means exit() never calls _proc_exit.
        if (!isError && !Asyncify.currData) {
          Module["onExit"]?.(EXITSTATUS);
        }
        // Track whether the return value was handled`;

if (doRewindOld.test(code)) {
  code = code.replace(doRewindOld, doRewindNew);
  console.log('Patched Asyncify doRewind completion');
  patched = true;
} else {
  console.error('Warning: Could not find Asyncify doRewind to patch');
}

if (patched) {
  fs.writeFileSync(path, code);
  console.log('Patches applied successfully to ' + path);
} else {
  console.error('No patches were applied');
  process.exit(1);
}
