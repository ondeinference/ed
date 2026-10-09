//! The client against an in-process stand-in for onde-web's API.

use std::sync::{Arc, Mutex};

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use ed_onde_account::{AccessToken, Error, OndeAccount, OndeEnv, SignIn};
use serde_json::{Value, json};
use std::collections::HashMap;

const TOKEN: &str = "jwt-123";

#[derive(Default)]
struct Mock {
    apps: Vec<Value>,
    /// Every `(method, path, env)` the server saw.
    calls: Vec<String>,
}

type Shared = Arc<Mutex<Mock>>;

fn app(id: &str, name: &str, status: &str) -> Value {
    json!({ "id": id, "name": name, "status": status, "app_id": id, "app_secret": format!("sec-{id}") })
}

fn authorized(headers: &HeaderMap) -> bool {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v == format!("Bearer {TOKEN}"))
}

async fn serve(apps: Vec<Value>) -> (OndeAccount, Shared) {
    let state: Shared = Arc::new(Mutex::new(Mock {
        apps,
        calls: vec![],
    }));

    async fn sign_in(
        State(s): State<Shared>,
        Query(q): Query<HashMap<String, String>>,
        Json(body): Json<Value>,
    ) -> (StatusCode, Json<Value>) {
        s.lock()
            .unwrap()
            .calls
            .push(format!("sign-in env={}", q["env"]));
        if body["email"] == "wrong@x.io" {
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({ "message": "Invalid password." })),
            );
        }
        (
            StatusCode::OK,
            Json(match body["email"].as_str().unwrap() {
                "ready@x.io" => {
                    json!({ "status": "ready", "accessToken": format!("Bearer {TOKEN}") })
                }
                "new@x.io" => json!({ "status": "notFound" }),
                _ => json!({ "status": "incomplete", "errorCode": 100 }),
            }),
        )
    }
    async fn me(headers: HeaderMap) -> (StatusCode, Json<Value>) {
        if !authorized(&headers) {
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({ "error": "Unauthorized" })),
            );
        }
        (
            StatusCode::OK,
            Json(json!({ "email": "ready@x.io", "name": "Ready", "can_create_gresiq_apps": true })),
        )
    }
    async fn list(State(s): State<Shared>, headers: HeaderMap) -> (StatusCode, Json<Value>) {
        if !authorized(&headers) {
            return (StatusCode::UNAUTHORIZED, Json(json!({})));
        }
        (
            StatusCode::OK,
            Json(Value::Array(s.lock().unwrap().apps.clone())),
        )
    }
    async fn create(State(s): State<Shared>, Json(body): Json<Value>) -> (StatusCode, Json<Value>) {
        let mut s = s.lock().unwrap();
        s.calls.push("create".into());
        let created = app(
            "new-1",
            body["gresiq_app"]["name"].as_str().unwrap(),
            "provisioning",
        );
        s.apps.push(created.clone());
        (StatusCode::CREATED, Json(created))
    }
    async fn activate(
        State(s): State<Shared>,
        Path(id): Path<String>,
        Json(body): Json<Value>,
    ) -> Json<Value> {
        let mut s = s.lock().unwrap();
        s.calls
            .push(format!("activate {id} {}", body["gresiq_app"]["status"]));
        let found = s.apps.iter_mut().find(|a| a["id"] == id).unwrap();
        found["status"] = json!("active");
        Json(found.clone())
    }

    let router = Router::new()
        .route("/api/auth/sign-in", post(sign_in))
        .route("/api/auth/me", get(me))
        .route("/api/gresiq/apps", get(list).post(create))
        .route("/api/gresiq/apps/{id}", patch(activate))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

    let account = OndeAccount::with_base_url(&format!("http://{addr}")).unwrap();
    (account, state)
}

#[tokio::test]
async fn sign_in_outcomes_and_token_normalization() {
    let (account, state) = serve(vec![]).await;

    let SignIn::Ready(token) = account.sign_in("ready@x.io", "pw").await.unwrap() else {
        panic!("expected ready");
    };
    assert_eq!(token.as_str(), TOKEN, "the Bearer prefix is stripped");
    assert_eq!(format!("{token:?}"), "AccessToken(<redacted>)");

    assert_eq!(
        account.sign_in("new@x.io", "pw").await.unwrap(),
        SignIn::NotFound
    );
    assert_eq!(
        account.sign_in("unconfirmed@x.io", "pw").await.unwrap(),
        SignIn::Incomplete {
            error_code: Some(100)
        }
    );

    assert!(matches!(
        account.sign_in("wrong@x.io", "pw").await,
        Err(Error::InvalidCredentials)
    ));

    let dev = account.clone().with_env(OndeEnv::Development);
    dev.sign_in("new@x.io", "pw").await.unwrap();
    let calls = state.lock().unwrap().calls.clone();
    assert_eq!(calls[0], "sign-in env=production");
    assert_eq!(calls[4], "sign-in env=development");
}

#[test]
fn access_token_strips_repeated_prefixes() {
    assert_eq!(AccessToken::new("  bearer Bearer abc ").as_str(), "abc");
    assert_eq!(AccessToken::new("abc").as_str(), "abc");
}

#[tokio::test]
async fn me_reads_the_profile() {
    let (account, _) = serve(vec![]).await;
    let profile = account.me(&AccessToken::new(TOKEN)).await.unwrap();
    assert_eq!(profile.email, "ready@x.io");
    assert!(profile.can_create);
}

#[tokio::test]
async fn ensure_key_reuses_an_active_app() {
    let (account, state) = serve(vec![
        app("a1", "Other", "active"),
        app("a2", "KaroKowe", "active"),
    ])
    .await;
    let key = account
        .ensure_key(&AccessToken::new(TOKEN), "KaroKowe")
        .await
        .unwrap();
    assert_eq!(key.as_str(), "a2:sec-a2");
    assert_eq!(format!("{key:?}"), "ApiKey(<redacted>)");
    assert!(
        state.lock().unwrap().calls.is_empty(),
        "nothing was created or patched"
    );
}

#[tokio::test]
async fn ensure_key_activates_a_provisioning_app() {
    let (account, state) = serve(vec![app("a1", "KaroKowe", "provisioning")]).await;
    let key = account
        .ensure_key(&AccessToken::new(TOKEN), "KaroKowe")
        .await
        .unwrap();
    assert_eq!(key.as_str(), "a1:sec-a1");
    assert_eq!(state.lock().unwrap().calls, vec![r#"activate a1 "active""#]);
}

#[tokio::test]
async fn ensure_key_creates_then_activates_when_none_exists() {
    let (account, state) = serve(vec![app("a1", "Other", "active")]).await;
    let token = AccessToken::new(TOKEN);
    let key = account.ensure_key(&token, "KaroKowe").await.unwrap();
    assert_eq!(key.as_str(), "new-1:sec-new-1");
    assert_eq!(
        state.lock().unwrap().calls,
        vec!["create".to_owned(), r#"activate new-1 "active""#.to_owned()]
    );
    // A second call finds it instead of creating another.
    account.ensure_key(&token, "KaroKowe").await.unwrap();
    assert_eq!(state.lock().unwrap().calls.len(), 2);
}

#[tokio::test]
async fn ensure_key_refuses_a_suspended_app() {
    let (account, _) = serve(vec![app("a1", "KaroKowe", "suspended")]).await;
    let err = account
        .ensure_key(&AccessToken::new(TOKEN), "KaroKowe")
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Suspended(name) if name == "KaroKowe"));
}

#[tokio::test]
async fn an_expired_token_is_unauthorized() {
    let (account, _) = serve(vec![]).await;
    let stale = AccessToken::new("expired");
    assert!(matches!(account.me(&stale).await, Err(Error::Unauthorized)));
    assert!(matches!(
        account.apps(&stale).await,
        Err(Error::Unauthorized)
    ));
    assert!(matches!(
        account.ensure_key(&stale, "KaroKowe").await,
        Err(Error::Unauthorized)
    ));
}
