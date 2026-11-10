//! Thread-safety bounds that relax on `wasm32`.
//!
//! The host-facing seams ([`ToolExecutor`](crate::ToolExecutor),
//! [`ApprovalHandler`](crate::ApprovalHandler), [`EventSink`](crate::EventSink),
//! [`SessionStore`](crate::SessionStore)) must be `Send + Sync` on native
//! targets, where a turn can hop between runtime threads. In a browser the
//! agent lives on one JS thread and its implementations hold `JsValue`s, which
//! are `!Send`. These aliases resolve to exactly `Send` / `Sync` everywhere
//! except `wasm32`, so native signatures are unchanged. It is the same pattern
//! wgpu uses (`WasmNotSend`).
//!
//! Soundness rests on the single-thread guarantee: nothing holding a `JsValue`
//! may be handed to another thread. Do not paper over a `!Send` type with
//! `unsafe impl Send`.

#[cfg(not(target_arch = "wasm32"))]
pub trait MaybeSend: Send {}
#[cfg(not(target_arch = "wasm32"))]
impl<T: Send + ?Sized> MaybeSend for T {}

#[cfg(target_arch = "wasm32")]
pub trait MaybeSend {}
#[cfg(target_arch = "wasm32")]
impl<T: ?Sized> MaybeSend for T {}

#[cfg(not(target_arch = "wasm32"))]
pub trait MaybeSync: Sync {}
#[cfg(not(target_arch = "wasm32"))]
impl<T: Sync + ?Sized> MaybeSync for T {}

#[cfg(target_arch = "wasm32")]
pub trait MaybeSync {}
#[cfg(target_arch = "wasm32")]
impl<T: ?Sized> MaybeSync for T {}
