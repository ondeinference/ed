//! Ed in the browser.
//!
//! [`WebChat`] wraps one [`Ed`] and is meant to live in a Web Worker: loading
//! the model and every token's forward pass block the thread they run on. The
//! page talks to it through `www/worker.js`.
//!
//! Each method returns a `Promise` and holds its own `Rc<Ed>`, because
//! wasm-bindgen would otherwise borrow `&self` across the whole await and a
//! second call (a `reset` while a reply streams) would panic.

use std::rc::Rc;

use ed_agent::{Ed, GgufModelConfig};
use js_sys::{Function, Promise};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::future_to_promise;

/// The model this example runs: the catalogue's smallest (about 380 MB). The
/// preset comes from onde, so no model name is written here.
fn model() -> GgufModelConfig {
    GgufModelConfig::qwen25_0_5b()
}

fn to_js_error(e: impl std::fmt::Display) -> JsValue {
    JsValue::from_str(&e.to_string())
}

fn number(n: u64) -> JsValue {
    JsValue::from_f64(n as f64)
}

#[wasm_bindgen]
pub struct WebChat {
    ed: Rc<Ed>,
}

impl Default for WebChat {
    fn default() -> Self {
        Self::new()
    }
}

#[wasm_bindgen]
impl WebChat {
    #[wasm_bindgen(constructor)]
    pub fn new() -> WebChat {
        console_error_panic_hook::set_once();
        WebChat {
            ed: Rc::new(Ed::new()),
        }
    }

    /// Display name and size of the model, for the first-run card.
    pub fn model_label(&self) -> String {
        let config = model();
        format!("{}, {}", config.display_name, config.approx_memory)
    }

    /// Resolves to whether the model is already in the browser cache.
    pub fn is_cached(&self) -> Promise {
        future_to_promise(async move {
            Ok(JsValue::from_bool(
                onde::web_store::is_cached(&model()).await,
            ))
        })
    }

    /// Download the model into the browser cache. `on_progress(received, total)`
    /// gets byte counts; `total` is `null` when the server hides the length.
    pub fn download(&self, on_progress: Function) -> Promise {
        future_to_promise(async move {
            onde::web_store::download_model(&model(), |received, total| {
                let total = total.map_or(JsValue::NULL, number);
                let _ = on_progress.call2(&JsValue::NULL, &number(received), &total);
            })
            .await
            .map_err(to_js_error)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Remove the downloaded model from the browser cache.
    pub fn delete_cached(&self) -> Promise {
        future_to_promise(async move {
            let removed = onde::web_store::delete_model(&model())
                .await
                .map_err(to_js_error)?;
            Ok(JsValue::from_bool(removed))
        })
    }

    /// Load the model into memory (downloading it first if it isn't cached).
    /// Resolves to the load time in seconds.
    pub fn load(&self, system_prompt: Option<String>) -> Promise {
        let ed = self.ed.clone();
        future_to_promise(async move {
            let took = ed
                .load(model(), system_prompt, None)
                .await
                .map_err(to_js_error)?;
            Ok(JsValue::from_f64(took.as_secs_f64()))
        })
    }

    /// Send one message. `on_delta(text)` is called for each piece as it is
    /// generated; the promise resolves with the whole reply.
    pub fn send(&self, message: String, on_delta: Function) -> Promise {
        let ed = self.ed.clone();
        future_to_promise(async move {
            let mut stream = ed.stream(message).await.map_err(to_js_error)?;
            let mut reply = String::new();
            while let Some(piece) = stream.next().await {
                let piece = piece.map_err(to_js_error)?;
                reply.push_str(&piece);
                let _ = on_delta.call1(&JsValue::NULL, &JsValue::from_str(&piece));
            }
            Ok(JsValue::from_str(&reply))
        })
    }

    /// Forget the conversation. Resolves to how many messages were dropped.
    pub fn reset(&self) -> Promise {
        let ed = self.ed.clone();
        future_to_promise(async move { Ok(number(ed.clear_history().await as u64)) })
    }

    /// Engine status, model name and history length.
    pub fn info(&self) -> Promise {
        let ed = self.ed.clone();
        future_to_promise(async move {
            serde_wasm_bindgen::to_value(&ed.info().await).map_err(to_js_error)
        })
    }
}
