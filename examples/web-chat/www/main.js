// UI thread: DOM and worker messaging only. All inference runs in worker.js.
// Messages are inserted with textContent, never as HTML.

const $ = (id) => document.getElementById(id);
const els = {
  unsupported: $("unsupported"),
  reason: $("unsupported-reason"),
  setup: $("setup"),
  setupTitle: $("setup-title"),
  setupText: $("setup-text"),
  download: $("download"),
  progressWrap: $("progress-wrap"),
  progress: $("progress"),
  progressLabel: $("progress-label"),
  chat: $("chat"),
  transcript: $("transcript"),
  composer: $("composer"),
  input: $("input"),
  send: $("send"),
  stop: $("stop"),
  stats: $("stats"),
  reset: $("reset"),
  clear: $("clear"),
};

const SYSTEM_PROMPT = "You are Ed, a concise and helpful assistant.";

// A minimal module that uses a v128 instruction; validate() is false where
// SIMD is missing.
const SIMD_PROBE = new Uint8Array([
  0, 97, 115, 109, 1, 0, 0, 0, 1, 5, 1, 96, 0, 1, 123, 3, 2, 1, 0, 10, 10, 1, 8,
  0, 65, 0, 253, 15, 253, 98, 11,
]);

let worker = null;
let modelLabel = "";
let nextId = 0;
let current = null; // { id, bubble }
let ready = false;
let resumeAfterReady = false; // set by Stop: reload the model without the setup card

const mb = (bytes) => `${(bytes / 1e6).toFixed(0)} MB`;

function show(section) {
  for (const name of ["unsupported", "setup", "chat"]) {
    els[name].hidden = name !== section;
  }
}

function unsupported(reason) {
  els.reason.textContent = reason;
  show("unsupported");
}

function addBubble(role, text = "") {
  const bubble = document.createElement("div");
  bubble.className = `bubble ${role}`;
  bubble.textContent = text;
  els.transcript.append(bubble);
  els.transcript.scrollTop = els.transcript.scrollHeight;
  return bubble;
}

function setBusy(busy) {
  els.send.hidden = busy;
  els.stop.hidden = !busy;
  els.input.disabled = busy;
  els.reset.disabled = busy;
  els.clear.disabled = busy;
}

function startWorker() {
  ready = false;
  worker = new Worker(new URL("./worker.js", import.meta.url), {
    type: "module",
  });
  worker.onmessage = ({ data }) => onMessage(data);
  worker.onerror = (e) => unsupported(`The worker failed to start: ${e.message}`);
}

function post(message) {
  worker.postMessage(message);
}

function showSetup(cached) {
  show("setup");
  els.progressWrap.hidden = true;
  els.download.disabled = false;
  els.setupTitle.textContent = cached ? "Load the model" : "Download the model";
  els.setupText.textContent = cached
    ? `${modelLabel} is saved in this browser. Loading takes a few seconds.`
    : `${modelLabel}. It downloads once, is saved in this browser, and runs entirely on this device.`;
  els.download.textContent = cached ? "Load" : "Download";
  els.download.dataset.cached = String(cached);
}

function onMessage(m) {
  switch (m.type) {
    case "ready":
      ready = true;
      modelLabel = m.model;
      if (resumeAfterReady) {
        resumeAfterReady = false;
        post({ type: "load", system: SYSTEM_PROMPT });
      } else {
        showSetup(m.cached);
      }
      break;
    case "progress": {
      els.progressWrap.hidden = false;
      if (m.total) {
        const pct = Math.min(100, (m.received / m.total) * 100);
        els.progress.value = pct;
        els.progressLabel.textContent = `${mb(m.received)} of ${mb(m.total)} (${pct.toFixed(0)}%)`;
      } else {
        els.progress.removeAttribute("value");
        els.progressLabel.textContent = `${mb(m.received)} downloaded`;
      }
      break;
    }
    case "downloaded":
      els.progressLabel.textContent = "Download complete. Loading the model…";
      post({ type: "load", system: SYSTEM_PROMPT });
      break;
    case "loaded":
      show("chat");
      setBusy(false);
      els.stats.textContent = `Model loaded in ${m.seconds.toFixed(1)} s`;
      els.input.focus();
      break;
    case "delta":
      if (current && current.id === m.id) {
        current.bubble.textContent += m.text;
        els.transcript.scrollTop = els.transcript.scrollHeight;
      }
      break;
    case "done":
      if (current && current.id === m.id) {
        current.bubble.classList.remove("pending");
        current = null;
        setBusy(false);
        const rate = m.pieces / Math.max(m.seconds, 0.001);
        els.stats.textContent = `${m.pieces} tokens in ${m.seconds.toFixed(1)} s, ${rate.toFixed(1)} tokens/s`;
        els.input.focus();
      }
      break;
    case "reset":
      els.transcript.replaceChildren();
      els.stats.textContent = "New conversation";
      break;
    case "cleared":
      els.transcript.replaceChildren();
      // The loaded model is still in memory; restart so it is released.
      restart();
      break;
    case "error":
      onError(m);
      break;
  }
}

function onError(m) {
  if (current) {
    current.bubble.classList.remove("pending");
    current.bubble.classList.add("error");
    const note = `Error: ${m.message}`;
    current.bubble.textContent = current.bubble.textContent
      ? `${current.bubble.textContent}\n\n[${note}]`
      : note;
    current = null;
    setBusy(false);
  } else if (m.stage === "boot") {
    unsupported(`Ed could not start: ${m.message}`);
  } else {
    show("setup");
    els.download.disabled = false;
    els.setupTitle.textContent = "Something went wrong";
    els.setupText.textContent = m.message;
  }
}

function restart() {
  worker.terminate();
  current = null;
  setBusy(false);
  show("setup");
  els.download.disabled = true;
  startWorker();
}

els.download.addEventListener("click", () => {
  els.download.disabled = true;
  if (els.download.dataset.cached === "true") {
    els.setupTitle.textContent = "Load the model";
    els.setupText.textContent = "Loading the model…";
    post({ type: "load", system: SYSTEM_PROMPT });
  } else {
    els.setupTitle.textContent = "Download the model";
    els.setupText.textContent = `${modelLabel}. Keep this tab open until it finishes.`;
    els.progress.value = 0;
    els.progressLabel.textContent = "Starting…";
    els.progressWrap.hidden = false;
    post({ type: "download" });
  }
});

els.composer.addEventListener("submit", (event) => {
  event.preventDefault();
  const text = els.input.value.trim();
  if (!text || current || !ready) return;
  els.input.value = "";
  addBubble("user", text);
  const bubble = addBubble("assistant");
  bubble.classList.add("pending");
  const id = nextId++;
  current = { id, bubble };
  setBusy(true);
  els.stats.textContent = "Thinking…";
  post({ type: "send", id, text });
});

els.input.addEventListener("keydown", (event) => {
  if (event.key === "Enter" && !event.shiftKey && !event.isComposing) {
    event.preventDefault();
    els.composer.requestSubmit();
  }
});

els.input.addEventListener("input", () => {
  els.input.style.height = "auto";
  els.input.style.height = `${els.input.scrollHeight}px`;
});

// Stop: a forward pass can't be interrupted from outside the worker, so end
// the worker and start another. The model is cached, so reloading is quick,
// but conversation history in the old worker is gone.
els.stop.addEventListener("click", () => {
  if (current) {
    current.bubble.classList.remove("pending");
    current.bubble.append(document.createTextNode(" [stopped]"));
  }
  worker.terminate();
  current = null;
  setBusy(false);
  els.stats.textContent = "Stopped. Reloading the model…";
  els.transcript.append(
    Object.assign(document.createElement("div"), {
      className: "bubble assistant",
      textContent: "Conversation history was reset when generation stopped.",
    }),
  );
  els.input.disabled = true;
  resumeAfterReady = true;
  startWorker();
});

els.reset.addEventListener("click", () => post({ type: "reset" }));
els.clear.addEventListener("click", () => {
  if (confirm("Remove the downloaded model from this browser?")) {
    post({ type: "clear" });
  }
});

if (!WebAssembly.validate(SIMD_PROBE)) {
  unsupported("It needs WebAssembly SIMD, which this browser does not support. Try a current Chrome, Edge, Firefox or Safari.");
} else if (!("caches" in self) || !window.isSecureContext) {
  unsupported("It needs Cache Storage, which requires a secure context. Open this page over https or from localhost.");
} else {
  startWorker();
}
