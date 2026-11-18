// Module worker: owns the wasm module and the one WebChat (Ed) instance.
// The page never touches inference, so a forward pass can't freeze the UI.
// Cancel works by terminating this worker and starting a new one; the model
// stays in Cache Storage, so the restart only re-reads it.

import init, { WebChat } from "./pkg/web_chat.js";

let chat = null;

const reply = (message) => self.postMessage(message);
const fail = (stage, error, id) =>
  reply({ type: "error", stage, id, message: String(error?.message ?? error) });

async function boot() {
  await init();
  chat = new WebChat();
  reply({
    type: "ready",
    model: chat.model_label(),
    cached: await chat.is_cached(),
  });
}

const handlers = {
  async download() {
    await chat.download((received, total) =>
      reply({ type: "progress", received, total }),
    );
    reply({ type: "downloaded" });
  },

  async load({ system }) {
    const seconds = await chat.load(system ?? undefined);
    reply({ type: "loaded", seconds });
  },

  async send({ id, text }) {
    const started = performance.now();
    let pieces = 0;
    const full = await chat.send(text, (delta) => {
      pieces += 1;
      reply({ type: "delta", id, text: delta });
    });
    reply({
      type: "done",
      id,
      text: full,
      pieces,
      seconds: (performance.now() - started) / 1000,
    });
  },

  async reset() {
    await chat.reset();
    reply({ type: "reset" });
  },

  async clear() {
    const removed = await chat.delete_cached();
    reply({ type: "cleared", removed });
  },
};

self.onmessage = async ({ data }) => {
  const handler = handlers[data.type];
  if (!handler) return;
  if (!chat) return fail(data.type, "Ed is still starting", data.id);
  try {
    await handler(data);
  } catch (error) {
    fail(data.type, error, data.id);
  }
};

boot().catch((error) => fail("boot", error));
