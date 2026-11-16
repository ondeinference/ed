# web-chat

Ed running a model entirely in the browser. The page talks to a Web Worker
that owns one `Ed` instance compiled to `wasm32-unknown-unknown`. Nothing you
type is sent to a server.

## Build and run

You need `wasm-pack` and the `wasm32-unknown-unknown` target.

```bash
./build.sh
python3 -m http.server -d www 8080
```

Open <http://localhost:8080>. No bundler, npm or special headers are needed.
Cache Storage needs a secure context, which `localhost` counts as.

The browser engine lives in `onde` 1.5.0. Until it is released, build against
a local checkout:

```bash
./build.sh --config 'patch.crates-io.onde.path="../../../onde"'
```

## What it downloads

Qwen 2.5 0.5B (GGUF, about 380 MB), once, on the first click of Download. It
is stored in the browser's Cache Storage for this origin, so a reload loads it
from disk without downloading again. **Remove model** in the footer deletes it.

## Speed

Inference is CPU only, single threaded, using WebAssembly SIMD. Expect one to three
tokens per second on a laptop (measured 1.2 to 2.5 on an Apple Silicon Mac). Stop ends the worker and starts a new one, so it
also resets the conversation.

## Layout

- `src/lib.rs`: the wasm-bindgen API (`WebChat`) around `Ed`.
- `www/worker.js`: owns the wasm module and the `WebChat`.
- `www/main.js`, `index.html`, `style.css`: the UI.

This crate is excluded from the root workspace and has its own lockfile.
