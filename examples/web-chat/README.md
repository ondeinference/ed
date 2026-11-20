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

The browser engine comes from `onde` 1.5.0 on crates.io. To try a local `onde`
checkout instead, pass a patch through to cargo:

```bash
./build.sh --config 'patch.crates-io.onde.path="../../../onde"'
```

## Deploy

The demo is published at <https://ondewebdemo.5mb.app> as a static site on
smbCloud (app `ondewebdemo`, in the Onde Inference project). It needs the `smb`
CLI 1.1.0 or newer and a prior `smb login`.

```bash
./build.sh
./deploy.sh --dry-run   # lists the files, uploads nothing
./deploy.sh             # publishes, then waits for /version.txt to match
```

`deploy.sh` ships only `index.html`, `main.js`, `worker.js`, `style.css`, the
icons, the two store badges in `badges/`, and the two files in `pkg/` that the
page loads. Each deploy is a new release, and
`/version.txt` holds the commit it was built from (`-dirty` if the tree had
uncommitted changes). The site is static and needs no special headers.

## What it downloads

Qwen 2.5 0.5B (GGUF, about 380 MB), once, on the first click of Download. It
is stored in the browser's Cache Storage for this origin, so a reload loads it
from disk without downloading again. **Remove model** in the footer deletes it.

## Phones and tablets

Loading the model peaks at about three times the file in memory (the cached
bytes, the copy handed to the engine, and the tensors built from it), so a bit
over 1 GB. Mobile browsers kill the tab before that. On phones and tablets the
page shows a warning instead of starting, with a link to Siti AI in the App
Store or Google Play (Onde in a native app) and a **Try anyway** button. The
same warning appears on any device whose previous visit was killed while the
model was loading. Lowering the peak is work for onde's `web_store`.

## Speed

Inference is CPU only, single threaded, using WebAssembly SIMD. Expect one to three
tokens per second on a laptop (measured 1.2 to 2.5 on an Apple Silicon Mac). Stop ends the worker and starts a new one, so it
also resets the conversation.

## Layout

- `src/lib.rs`: the wasm-bindgen API (`WebChat`) around `Ed`.
- `www/worker.js`: owns the wasm module and the `WebChat`.
- `www/main.js`, `index.html`, `style.css`: the UI.

This crate is excluded from the root workspace and has its own lockfile.
