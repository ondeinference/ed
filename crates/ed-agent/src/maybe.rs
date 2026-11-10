//! Thread-safety bounds that relax on browser wasm.
//!
//! The host-facing seams ([`ToolExecutor`](crate::ToolExecutor),
//! [`ApprovalHandler`](crate::ApprovalHandler), [`EventSink`](crate::EventSink),
//! [`SessionStore`](crate::SessionStore)) must be `Send + Sync` on native
//! targets, where a turn can hop between runtime threads. In a browser the
//! agent lives on one JS thread and its implementations hold `JsValue`s, which
//! are `!Send`. These aliases resolve to exactly `Send` / `Sync` everywhere
//! except `wasm32-unknown-unknown`, so native signatures are unchanged. Other
//! wasm targets (`wasm32-wasip1-threads`, Emscripten with pthreads) can run
//! threads, so they keep the real bounds. It is the same pattern
//! wgpu uses (`WasmNotSend`).
//!
//! Soundness rests on the single-thread guarantee: nothing holding a `JsValue`
//! may be handed to another thread. Do not paper over a `!Send` type with
//! `unsafe impl Send`.

#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub trait MaybeSend: Send {}
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
impl<T: Send + ?Sized> MaybeSend for T {}

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
pub trait MaybeSend {}
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
impl<T: ?Sized> MaybeSend for T {}

#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub trait MaybeSync: Sync {}
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
impl<T: Sync + ?Sized> MaybeSync for T {}

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
pub trait MaybeSync {}
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
impl<T: ?Sized> MaybeSync for T {}
