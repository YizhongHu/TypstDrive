use super::{db_error, Error};
use crate::AppState;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::Redirect,
    Json,
};
use axum_extra::extract::cookie::SignedCookieJar;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use cookie::{Cookie, CookieJar};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    io::Read,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[derive(Clone)]
pub struct Pending {
    pub user: String,
    pub verifier: String,
    pub expires: i64,
}
pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
pub fn user(jar: &SignedCookieJar) -> Result<String, Error> {
    jar.get("session_user_id")
        .map(|c| c.value().to_string())
        .ok_or((
            StatusCode::UNAUTHORIZED,
            "Sign in to TypstDrive first".into(),
        ))
}
fn configured() -> bool {
    [
        "GITHUB_CLIENT_ID",
        "GITHUB_CLIENT_SECRET",
        "GITHUB_CALLBACK_URL",
        "GITHUB_APP_SLUG",
    ]
    .iter()
    .all(|k| std::env::var(k).map(|s| !s.is_empty()).unwrap_or(false))
        && std::env::var("COOKIE_SECRET")
            .map(|s| s.len() >= 64)
            .unwrap_or(false)
}
fn installation_url() -> Option<String> {
    let slug = std::env::var("GITHUB_APP_SLUG").ok()?;
    if slug.is_empty() || !slug.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        return None;
    }
    Some(format!("https://github.com/apps/{slug}/installations/new"))
}
pub fn seal(state: &AppState, value: &str) -> String {
    let mut jar = CookieJar::new();
    jar.private_mut(&state.key)
        .add(Cookie::new("github", value.to_string()));
    jar.get("github").unwrap().value().to_string()
}
fn unseal(state: &AppState, value: String) -> Result<String, Error> {
    let mut jar = CookieJar::new();
    jar.add_original(Cookie::new("github", value));
    jar.private(&state.key)
        .get("github")
        .map(|c| c.value().to_string())
        .ok_or((
            StatusCode::UNAUTHORIZED,
            "Reconnect GitHub; the stored connection cannot be decrypted".into(),
        ))
}
pub async fn status(
    State(state): State<AppState>,
    jar: SignedCookieJar,
) -> Result<Json<Value>, Error> {
    let uid = user(&jar)?;
    let account = sqlx::query_as::<_, (String, i64)>(
        "SELECT login, expires_at FROM github_accounts WHERE user_id = ?",
    )
    .bind(uid)
    .fetch_optional(&state.db)
    .await
    .map_err(db_error)?;
    Ok(Json(
        json!({"configured":configured(),"connected":account.is_some(),"login":account.as_ref().map(|a| &a.0),"expires_at":account.map(|a|a.1),"installation_url":installation_url()}),
    ))
}
pub async fn disconnect(
    State(state): State<AppState>,
    jar: SignedCookieJar,
) -> Result<StatusCode, Error> {
    let uid = user(&jar)?;
    let _gate = state.github.account_gate.lock().await;
    state
        .github
        .pending
        .lock()
        .await
        .retain(|_, p| p.user != uid);
    sqlx::query("DELETE FROM github_accounts WHERE user_id = ?")
        .bind(uid)
        .execute(&state.db)
        .await
        .map_err(db_error)?;
    Ok(StatusCode::NO_CONTENT)
}
pub async fn start(State(state): State<AppState>, jar: SignedCookieJar) -> Result<Redirect, Error> {
    let uid = user(&jar)?;
    if !configured() {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "The administrator must configure the GitHub App and a stable cookie key".into(),
        ));
    }
    let nonce = uuid::Uuid::new_v4().to_string();
    let verifier = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let mut pending = state.github.pending.lock().await;
    pending.retain(|_, v| v.expires > now() && v.user != uid);
    if pending.len() >= 1000 {
        return Err((
            StatusCode::TOO_MANY_REQUESTS,
            "Too many pending GitHub connections".into(),
        ));
    }
    pending.insert(
        nonce.clone(),
        Pending {
            user: uid,
            verifier,
            expires: now() + 600,
        },
    );
    let uri = std::env::var("GITHUB_CALLBACK_URL").unwrap();
    if !uri.starts_with("https://") {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "GitHub callback must use HTTPS".into(),
        ));
    }
    let url = format!("https://github.com/login/oauth/authorize?client_id={}&redirect_uri={}&state={nonce}&code_challenge={challenge}&code_challenge_method=S256&scope=offline_access&allow_signup=false",super::client::url_segment(&std::env::var("GITHUB_CLIENT_ID").unwrap()),super::client::url_segment(&uri));
    Ok(Redirect::to(&url))
}
#[derive(Deserialize)]
pub struct Callback {
    pub code: Option<String>,
    pub state: Option<String>,
}
fn exchange(mut body: Value) -> Result<Value, String> {
    body["client_id"] =
        json!(std::env::var("GITHUB_CLIENT_ID").map_err(|_| "GitHub App not configured")?);
    body["client_secret"] =
        json!(std::env::var("GITHUB_CLIENT_SECRET").map_err(|_| "GitHub App not configured")?);
    let response = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(30))
        .redirects(0)
        .build()
        .post("https://github.com/login/oauth/access_token")
        .set("Accept", "application/json")
        .set("Content-Type", "application/json")
        .send_string(&body.to_string())
        .map_err(|_| "GitHub authorization failed")?;
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(65537)
        .read_to_end(&mut bytes)
        .map_err(|_| "Cannot read GitHub authorization")?;
    if bytes.len() > 65536 {
        return Err("Invalid GitHub authorization response".into());
    }
    let value: Value =
        serde_json::from_slice(&bytes).map_err(|_| "Invalid GitHub authorization response")?;
    if value["access_token"].as_str().is_none() {
        return Err("GitHub authorization expired or was denied; reconnect".into());
    }
    Ok(value)
}
async fn store(state: &AppState, uid: &str, login: &str, value: &Value) -> Result<(), Error> {
    let encrypted = seal(
        state,
        value["access_token"]
            .as_str()
            .ok_or((StatusCode::BAD_GATEWAY, "GitHub token missing".into()))?,
    );
    let refresh = value["refresh_token"]
        .as_str()
        .map(|s| seal(state, s))
        .unwrap_or_default();
    let expiry = value["expires_in"]
        .as_i64()
        .map(|s| now() + s)
        .unwrap_or(i64::MAX);
    sqlx::query("INSERT INTO github_accounts (user_id,login,token,refresh_token,expires_at) VALUES (?,?,?,?,?) ON CONFLICT(user_id) DO UPDATE SET login=excluded.login,token=excluded.token,refresh_token=excluded.refresh_token,expires_at=excluded.expires_at")
        .bind(uid).bind(login).bind(encrypted).bind(refresh).bind(expiry).execute(&state.db).await.map_err(db_error)?;
    Ok(())
}
pub async fn callback(
    State(state): State<AppState>,
    jar: SignedCookieJar,
    Query(query): Query<Callback>,
) -> Result<Redirect, Error> {
    let uid = user(&jar)?;
    let nonce = query.state.ok_or((
        StatusCode::BAD_REQUEST,
        "Missing GitHub authorization state".into(),
    ))?;
    let pending = state
        .github
        .pending
        .lock()
        .await
        .get(&nonce)
        .cloned()
        .ok_or((
            StatusCode::BAD_REQUEST,
            "GitHub connection expired; try again".into(),
        ))?;
    if pending.user != uid || pending.expires < now() {
        return Err((
            StatusCode::FORBIDDEN,
            "GitHub authorization does not match this account".into(),
        ));
    }
    let code = query.code.ok_or((
        StatusCode::BAD_REQUEST,
        "GitHub authorization was declined".into(),
    ))?;
    let value = tokio::task::spawn_blocking(move || exchange(json!({"code":code,"code_verifier":pending.verifier,"redirect_uri":std::env::var("GITHUB_CALLBACK_URL").unwrap_or_default()}))).await.map_err(|_|(StatusCode::BAD_GATEWAY,"GitHub authorization failed".into()))?.map_err(|e|(StatusCode::BAD_GATEWAY,e))?;
    let client = super::client::Client::new(value["access_token"].as_str().unwrap().to_string());
    let profile = tokio::task::spawn_blocking(move || client.request("GET", "/user", None))
        .await
        .map_err(|_| {
            (
                StatusCode::BAD_GATEWAY,
                "GitHub profile lookup failed".into(),
            )
        })?
        .map_err(|e| (StatusCode::BAD_GATEWAY, e))?;
    let login = profile["login"]
        .as_str()
        .ok_or((StatusCode::BAD_GATEWAY, "GitHub profile missing".into()))?;
    let _sync = state.github.sync_gate.lock().await;
    let _gate = state.github.account_gate.lock().await;
    let exists = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM users WHERE id=?")
        .bind(&uid)
        .fetch_one(&state.db)
        .await
        .map_err(db_error)?;
    if exists != 1 {
        return Err((
            StatusCode::UNAUTHORIZED,
            "This TypstDrive account is no longer approved".into(),
        ));
    }
    let active = state
        .github
        .pending
        .lock()
        .await
        .remove(&nonce)
        .map(|p| p.expires >= now() && p.user == uid)
        .unwrap_or(false);
    if !active {
        return Err((
            StatusCode::BAD_REQUEST,
            "GitHub connection was cancelled or expired".into(),
        ));
    }
    store(&state, &uid, login, &value).await?;
    Ok(Redirect::to("/settings?github=connected"))
}
pub async fn token(state: &AppState, uid: &str) -> Result<String, Error> {
    let _gate = state.github.account_gate.lock().await;
    let (login, token, refresh, expiry) = sqlx::query_as::<_, (String, String, String, i64)>(
        "SELECT login,token,refresh_token,expires_at FROM github_accounts WHERE user_id = ?",
    )
    .bind(uid)
    .fetch_optional(&state.db)
    .await
    .map_err(db_error)?
    .ok_or((
        StatusCode::UNAUTHORIZED,
        "Connect your GitHub account in settings".into(),
    ))?;
    if expiry > now() + 60 {
        return unseal(state, token);
    }
    if refresh.is_empty() {
        return Err((
            StatusCode::UNAUTHORIZED,
            "Reconnect your expired GitHub account".into(),
        ));
    }
    let refresh = unseal(state, refresh)?;
    let value = tokio::task::spawn_blocking(move || {
        exchange(json!({"grant_type":"refresh_token","refresh_token":refresh}))
    })
    .await
    .map_err(|_| (StatusCode::BAD_GATEWAY, "GitHub refresh failed".into()))?
    .map_err(|e| (StatusCode::UNAUTHORIZED, e))?;
    store(state, uid, &login, &value).await?;
    Ok(value["access_token"].as_str().unwrap().to_string())
}
