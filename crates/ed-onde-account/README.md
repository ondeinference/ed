# ed-onde-account

An Onde Inference account client for [Ed](https://crates.io/crates/ed-agent) agents.

It signs a user in to their Onde account, finds or creates their app, activates it, and returns
the Onde Cloud API key (`app_id:app_secret`) an agent authenticates with. Every call goes through
the ondeinference.com API, which keeps Onde's smbCloud credentials on the server, so nothing
secret ships in the app that embeds this crate.

```rust
use ed_onde_account::{OndeAccount, SignIn};

let account = OndeAccount::new(); // https://ondeinference.com, production
match account.sign_in("me@example.com", "password").await? {
    SignIn::Ready(token) => {
        // Reuses the app named "My Agent", or creates and activates it.
        let key = account.ensure_key(&token, "My Agent").await?;
        println!("{}", key.as_str()); // app_id:app_secret
    }
    SignIn::NotFound => { /* no account yet */ }
    SignIn::Incomplete { .. } => { /* e.g. the email isn't confirmed */ }
}
```

- `sign_in`, `me`, `apps`, `create_app`, `activate` and `ensure_key`.
- `ensure_key` is idempotent: it never creates a second app with the same name.
- Errors are typed. `Error::Unauthorized` means the token expired and the user must sign in again;
  `Error::Forbidden` means the account may not create apps.
- `AccessToken` and `ApiKey` redact themselves in `Debug` output.

Creating apps needs an Onde account that is allowed to, which every signed-up user is.

## License

MIT OR Apache-2.0
