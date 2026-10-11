//! The seam between Ed and a host's chat storage.
//!
//! Ed owns no storage. A host that keeps several conversations implements
//! [`SessionStore`] over whatever it already has (files, SQLite, a keychain)
//! and hands the messages to [`Ed::restore_history`](crate::Ed::restore_history)
//! when the user switches conversation.

use async_trait::async_trait;
use onde::inference::types::ChatMessage;
use serde::{Deserialize, Serialize};

/// What a session list needs to render a row, without loading the messages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMeta {
    pub id: String,
    pub title: String,
    /// Unix seconds.
    pub created_at: i64,
    /// Unix seconds.
    pub updated_at: i64,
    pub message_count: usize,
}

/// A storage failure. The store decides the wording; Ed only carries it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct SessionError(pub String);

/// Persistent conversations, keyed by an id the host chooses.
///
/// Contract:
/// - `load` of an unknown id is `Ok(None)`, not an error.
/// - `save` creates the session if it doesn't exist and replaces its
///   messages if it does.
/// - `delete` of an unknown id succeeds.
/// - `list` is ordered most recently updated first.
#[async_trait]
pub trait SessionStore: Send + Sync + 'static {
    async fn list(&self) -> Result<Vec<SessionMeta>, SessionError>;
    async fn load(&self, id: &str) -> Result<Option<Vec<ChatMessage>>, SessionError>;
    async fn save(&self, id: &str, messages: &[ChatMessage]) -> Result<(), SessionError>;
    async fn delete(&self, id: &str) -> Result<(), SessionError>;
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::*;

    struct Stored {
        messages: Vec<ChatMessage>,
        created: i64,
        updated: i64,
    }

    /// The reference implementation of the contract above.
    #[derive(Default)]
    struct MemoryStore {
        sessions: Mutex<HashMap<String, Stored>>,
        clock: Mutex<i64>,
    }

    impl MemoryStore {
        fn tick(&self) -> i64 {
            let mut clock = self.clock.lock().unwrap();
            *clock += 1;
            *clock
        }
    }

    #[async_trait]
    impl SessionStore for MemoryStore {
        async fn list(&self) -> Result<Vec<SessionMeta>, SessionError> {
            let mut metas: Vec<_> = self
                .sessions
                .lock()
                .unwrap()
                .iter()
                .map(|(id, stored)| SessionMeta {
                    id: id.clone(),
                    title: stored
                        .messages
                        .first()
                        .map(|m| m.content.clone())
                        .unwrap_or_default(),
                    created_at: stored.created,
                    updated_at: stored.updated,
                    message_count: stored.messages.len(),
                })
                .collect();
            metas.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
            Ok(metas)
        }

        async fn load(&self, id: &str) -> Result<Option<Vec<ChatMessage>>, SessionError> {
            Ok(self
                .sessions
                .lock()
                .unwrap()
                .get(id)
                .map(|s| s.messages.clone()))
        }

        async fn save(&self, id: &str, messages: &[ChatMessage]) -> Result<(), SessionError> {
            let now = self.tick();
            let mut sessions = self.sessions.lock().unwrap();
            let created = sessions.get(id).map_or(now, |s| s.created);
            sessions.insert(
                id.to_owned(),
                Stored {
                    messages: messages.to_vec(),
                    created,
                    updated: now,
                },
            );
            Ok(())
        }

        async fn delete(&self, id: &str) -> Result<(), SessionError> {
            self.sessions.lock().unwrap().remove(id);
            Ok(())
        }
    }

    #[tokio::test]
    async fn loading_an_unknown_session_is_none() {
        let store = MemoryStore::default();
        assert!(store.load("nope").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn save_creates_then_replaces() {
        let store = MemoryStore::default();
        store.save("a", &[ChatMessage::user("hi")]).await.unwrap();
        store
            .save(
                "a",
                &[ChatMessage::user("hi"), ChatMessage::assistant("yo")],
            )
            .await
            .unwrap();

        let loaded = store.load("a").await.unwrap().unwrap();
        assert_eq!(loaded.len(), 2);
        let listed = store.list().await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].message_count, 2);
        assert!(listed[0].updated_at > listed[0].created_at);
    }

    #[tokio::test]
    async fn list_is_most_recently_updated_first() {
        let store = MemoryStore::default();
        store.save("old", &[ChatMessage::user("1")]).await.unwrap();
        store.save("new", &[ChatMessage::user("2")]).await.unwrap();
        store.save("old", &[ChatMessage::user("3")]).await.unwrap();

        let ids: Vec<_> = store
            .list()
            .await
            .unwrap()
            .into_iter()
            .map(|m| m.id)
            .collect();
        assert_eq!(ids, ["old", "new"]);
    }

    #[tokio::test]
    async fn delete_is_idempotent() {
        let store = MemoryStore::default();
        store.save("a", &[]).await.unwrap();
        store.delete("a").await.unwrap();
        store.delete("a").await.unwrap();
        assert!(store.list().await.unwrap().is_empty());
    }
}
