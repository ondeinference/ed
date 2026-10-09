//! A small client for the Onde Inference account API at ondeinference.com.
//!
//! It signs a user in, finds or creates their app, activates it and returns the Onde Cloud
//! API key (`app_id:app_secret`) an agent authenticates with. Every call goes through
//! onde-web's own API, which adds the smbCloud client credentials on the server, so no
//! Onde secret ships in whatever embeds this crate.
//!
//! ```no_run
//! # async fn run() -> Result<(), ed_onde_account::Error> {
//! use ed_onde_account::{OndeAccount, SignIn};
//!
//! let account = OndeAccount::new();
//! if let SignIn::Ready(token) = account.sign_in("me@example.com", "password").await? {
//!     let key = account.ensure_key(&token, "My Agent").await?;
//!     // key.as_str() is "app_id:app_secret"
//! }
//! # Ok(())
//! # }
//! ```

use std::fmt;

use reqwest::{Method, RequestBuilder, StatusCode, Url};
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};

/// The production API host.
pub const DEFAULT_BASE_URL: &str = "https://ondeinference.com";

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The access token is missing, expired or revoked. Sign in again.
    #[error("not signed in, or the session expired")]
    Unauthorized,
    /// Sign-in was refused: the email and password don't match.
    #[error("incorrect email or password")]
    InvalidCredentials,
    /// Signed in, but this account may not do that.
    #[error("this Onde account is not allowed to do that")]
    Forbidden,
    /// The user's app with this name exists but is suspended, so it cannot get a key.
    #[error("the app \"{0}\" is suspended")]
    Suspended(String),
    /// The API answered with another failure status.
    #[error("Onde API error {status}: {message}")]
    Http { status: u16, message: String },
    /// The API answered, but not with the shape this client expects.
    #[error("unexpected response from the Onde API: {0}")]
    Unexpected(String),
    /// The request never completed.
    #[error("could not reach the Onde API: {0}")]
    Transport(#[from] reqwest::Error),
}

/// Which Onde backend to talk to. Production unless a host is testing against the dev stack.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum OndeEnv {
    #[default]
    Production,
    Development,
}

impl OndeEnv {
    fn as_str(self) -> &'static str {
        match self {
            Self::Production => "production",
            Self::Development => "development",
        }
    }
}

/// The smbCloud access token for a signed-in user. Debug output is redacted.
#[derive(Clone, PartialEq, Eq)]
pub struct AccessToken(String);

impl AccessToken {
    /// Wrap a token, stripping surrounding whitespace and any `Bearer ` prefixes.
    pub fn new(token: impl AsRef<str>) -> Self {
        let mut token = token.as_ref().trim();
        while let Some(prefix) = token.get(..7) {
            if !prefix.eq_ignore_ascii_case("bearer ") {
                break;
            }
            token = token[7..].trim_start();
        }
        Self(token.to_owned())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for AccessToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AccessToken(<redacted>)")
    }
}

/// An Onde Cloud API key, `app_id:app_secret`. Debug output is redacted.
#[derive(Clone, PartialEq, Eq)]
pub struct ApiKey(String);

impl ApiKey {
    pub fn new(app_id: &str, app_secret: &str) -> Self {
        Self(format!("{app_id}:{app_secret}"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApiKey(<redacted>)")
    }
}

/// How a sign-in attempt ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignIn {
    Ready(AccessToken),
    /// No account for this email.
    NotFound,
    /// The account exists but cannot sign in yet, for instance an unconfirmed email.
    Incomplete {
        error_code: Option<i64>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AppStatus {
    Provisioning,
    Active,
    Suspended,
    #[serde(other)]
    Unknown,
}

/// An Onde app, the thing an API key belongs to.
#[derive(Clone, Deserialize)]
pub struct OndeApp {
    #[serde(deserialize_with = "string_or_number")]
    pub id: String,
    pub name: String,
    pub status: AppStatus,
    #[serde(default, deserialize_with = "opt_string_or_number")]
    pub app_id: Option<String>,
    pub app_secret: Option<String>,
}

impl fmt::Debug for OndeApp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OndeApp")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("status", &self.status)
            .field(
                "app_secret",
                &self.app_secret.as_ref().map(|_| "<redacted>"),
            )
            .finish()
    }
}

impl OndeApp {
    /// The key for this app, if the secret is present.
    pub fn api_key(&self) -> Option<ApiKey> {
        let secret = self.app_secret.as_deref().filter(|s| !s.is_empty())?;
        Some(ApiKey::new(
            self.app_id.as_deref().unwrap_or(&self.id),
            secret,
        ))
    }
}

/// The signed-in user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    pub email: String,
    pub name: Option<String>,
    /// Whether the account may create apps (and so keys).
    pub can_create: bool,
}

#[derive(Debug, Clone)]
pub struct OndeAccount {
    http: reqwest::Client,
    base_url: Url,
    env: OndeEnv,
}

impl Default for OndeAccount {
    fn default() -> Self {
        Self::new()
    }
}

impl OndeAccount {
    /// Production at ondeinference.com.
    pub fn new() -> Self {
        Self::with_base_url(DEFAULT_BASE_URL).expect("the default base URL is valid")
    }

    /// Point at another host, such as a local onde-web or a test server.
    pub fn with_base_url(base_url: &str) -> Result<Self> {
        let base_url = Url::parse(base_url)
            .map_err(|e| Error::Unexpected(format!("invalid base URL: {e}")))?;
        Ok(Self {
            http: reqwest::Client::new(),
            base_url,
            env: OndeEnv::default(),
        })
    }

    pub fn with_env(mut self, env: OndeEnv) -> Self {
        self.env = env;
        self
    }

    pub async fn sign_in(&self, email: &str, password: &str) -> Result<SignIn> {
        let body = json!({ "email": email.trim(), "password": password });
        let value = self
            .send(self.request(Method::POST, "/api/auth/sign-in").json(&body))
            .await
            // There is no token to expire yet, so a 401 here is a wrong password.
            .map_err(|e| match e {
                Error::Unauthorized => Error::InvalidCredentials,
                e => e,
            })?;
        match value.get("status").and_then(Value::as_str) {
            Some("ready") => match value.get("accessToken").and_then(Value::as_str) {
                Some(token) if !AccessToken::new(token).as_str().is_empty() => {
                    Ok(SignIn::Ready(AccessToken::new(token)))
                }
                _ => Err(Error::Unexpected(
                    "sign-in was ready without a token".into(),
                )),
            },
            Some("notFound") => Ok(SignIn::NotFound),
            Some("incomplete") => Ok(SignIn::Incomplete {
                error_code: value.get("errorCode").and_then(Value::as_i64),
            }),
            _ => Err(Error::Unexpected("unknown sign-in status".into())),
        }
    }

    pub async fn me(&self, token: &AccessToken) -> Result<Profile> {
        let value = self
            .send(
                self.request(Method::GET, "/api/auth/me")
                    .bearer_auth(token.as_str()),
            )
            .await?;
        let flag = |key: &str| value.get(key).and_then(Value::as_bool);
        Ok(Profile {
            email: value
                .get("email")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            name: value.get("name").and_then(Value::as_str).map(str::to_owned),
            can_create: flag("can_create_gresiq_apps")
                .or_else(|| flag("can_manage_all_gresiq_apps"))
                .unwrap_or(false),
        })
    }

    pub async fn apps(&self, token: &AccessToken) -> Result<Vec<OndeApp>> {
        let value = self
            .send(
                self.request(Method::GET, "/api/gresiq/apps")
                    .bearer_auth(token.as_str()),
            )
            .await?;
        decode(value)
    }

    pub async fn create_app(&self, token: &AccessToken, name: &str) -> Result<OndeApp> {
        let value = self
            .send(
                self.request(Method::POST, "/api/gresiq/apps")
                    .bearer_auth(token.as_str())
                    .json(&json!({ "gresiq_app": { "name": name } })),
            )
            .await?;
        decode(value)
    }

    /// Move a provisioning app to `active`. Onde Cloud refuses its key until then.
    pub async fn activate(&self, token: &AccessToken, id: &str) -> Result<OndeApp> {
        let value = self
            .send(
                self.request(Method::PATCH, &format!("/api/gresiq/apps/{id}"))
                    .bearer_auth(token.as_str())
                    .json(&json!({ "gresiq_app": { "status": "active" } })),
            )
            .await?;
        decode(value)
    }

    /// Reuse the user's app named `name` (or create it), activate it if needed and return
    /// its key. Safe to call again: it never creates a second app with the same name.
    pub async fn ensure_key(&self, token: &AccessToken, name: &str) -> Result<ApiKey> {
        let apps = self.apps(token).await?;
        let named = || apps.iter().filter(|a| a.name == name);
        let existing = named()
            .find(|a| a.status == AppStatus::Active)
            .or_else(|| named().find(|a| a.status != AppStatus::Suspended))
            .cloned();

        let mut app = match existing {
            Some(app) => app,
            // Whatever is left under this name is suspended.
            None if named().next().is_some() => return Err(Error::Suspended(name.to_owned())),
            None => self.create_app(token, name).await?,
        };
        if app.status != AppStatus::Active {
            app = self.activate(token, &app.id).await?;
        }
        app.api_key()
            .ok_or_else(|| Error::Unexpected("the app came back without a secret".into()))
    }

    fn request(&self, method: Method, path: &str) -> RequestBuilder {
        let mut url = self.base_url.clone();
        url.set_path(path);
        url.query_pairs_mut().append_pair("env", self.env.as_str());
        self.http.request(method, url)
    }

    async fn send(&self, request: RequestBuilder) -> Result<Value> {
        let response = request.send().await?;
        let status = response.status();
        let text = response.text().await?;
        let body: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        if status.is_success() {
            return Ok(body);
        }
        Err(match status {
            StatusCode::UNAUTHORIZED => Error::Unauthorized,
            StatusCode::FORBIDDEN => Error::Forbidden,
            _ => Error::Http {
                status: status.as_u16(),
                message: ["message", "error"]
                    .iter()
                    .find_map(|k| body.get(*k).and_then(Value::as_str))
                    .map(str::to_owned)
                    .or_else(|| {
                        body.get("errors")
                            .and_then(Value::as_array)
                            .map(|e| {
                                e.iter()
                                    .filter_map(Value::as_str)
                                    .collect::<Vec<_>>()
                                    .join("; ")
                            })
                            .filter(|m| !m.is_empty())
                    })
                    .unwrap_or_else(|| "request failed".to_owned()),
            },
        })
    }
}

fn decode<T: for<'de> Deserialize<'de>>(value: Value) -> Result<T> {
    serde_json::from_value(value).map_err(|e| Error::Unexpected(e.to_string()))
}

fn string_or_number<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    match Value::deserialize(d)? {
        Value::String(s) => Ok(s),
        Value::Number(n) => Ok(n.to_string()),
        other => Err(serde::de::Error::custom(format!(
            "expected an id, got {other}"
        ))),
    }
}

fn opt_string_or_number<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    match Option::<Value>::deserialize(d)? {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(Value::Number(n)) => Ok(Some(n.to_string())),
        Some(other) => Err(serde::de::Error::custom(format!(
            "expected an id, got {other}"
        ))),
    }
}
