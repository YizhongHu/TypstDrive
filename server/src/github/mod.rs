pub mod auth;
pub mod client;
use crate::AppState;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use axum_extra::extract::cookie::SignedCookieJar;
use client::{Files, Target};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use tokio::sync::Mutex;
use yrs::updates::decoder::Decode;
use yrs::{Doc, GetString, ReadTxn, StateVector, Text, Transact, Update};

pub type Error = (StatusCode, String);
#[derive(Default)]
pub struct Runtime {
    #[cfg(test)]
    pub test_base: Option<String>,
    pub pending: Mutex<HashMap<String, auth::Pending>>,
    pub account_gate: Mutex<()>,
    // Serialize sync operations; collaborative edits continue through Yjs.
    pub sync_gate: Mutex<()>,
}
pub fn db_error(_: sqlx::Error) -> Error {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "Unable to save GitHub sync state".into(),
    )
}
fn bad(message: &str) -> Error {
    (StatusCode::BAD_REQUEST, message.into())
}
fn conflict(message: &str) -> Error {
    (StatusCode::CONFLICT, message.into())
}
fn hashes(files: &Files) -> BTreeMap<String, String> {
    files
        .iter()
        .map(|(p, b)| (p.clone(), format!("{:x}", Sha256::digest(b))))
        .collect()
}
fn fingerprint(files: &Files) -> String {
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&hashes(files)).unwrap())
    )
}
fn retain_tracked_previous(
    previous: &mut Files,
    local: &Files,
    baseline: &BTreeMap<String, String>,
) {
    // A first push must not delete selected remote assets never pulled locally.
    previous.retain(|path, _| local.contains_key(path) || baseline.contains_key(path));
}

pub async fn init_schema(db: &sqlx::AnyPool) {
    for statement in [
        "CREATE TABLE IF NOT EXISTS github_accounts (user_id TEXT PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,login TEXT NOT NULL,token TEXT NOT NULL,refresh_token TEXT NOT NULL,expires_at BIGINT NOT NULL)",
        "CREATE TABLE IF NOT EXISTS github_links (kind TEXT NOT NULL,resource_id TEXT NOT NULL,target TEXT NOT NULL,head TEXT NOT NULL,base_hashes TEXT NOT NULL,PRIMARY KEY(kind,resource_id))",
    ] { sqlx::query(statement).execute(db).await.expect("Unable to initialize GitHub sync schema"); }
}

#[derive(Clone, Serialize, Deserialize)]
struct Link {
    target: Target,
    head: String,
    base_hashes: BTreeMap<String, String>,
}
async fn link(state: &AppState, kind: &str, id: &str) -> Result<Option<Link>, Error> {
    let row = sqlx::query_as::<_, (String, String, String)>(
        "SELECT target,head,base_hashes FROM github_links WHERE kind=? AND resource_id=?",
    )
    .bind(kind)
    .bind(id)
    .fetch_optional(&state.db)
    .await
    .map_err(db_error)?;
    row.map(|(target, head, base)| {
        Ok(Link {
            target: serde_json::from_str(&target)
                .map_err(|_| bad("Invalid stored GitHub connection"))?,
            head,
            base_hashes: serde_json::from_str(&base)
                .map_err(|_| bad("Invalid stored sync baseline"))?,
        })
    })
    .transpose()
}
async fn authorize(
    state: &AppState,
    jar: &SignedCookieJar,
    kind: &str,
    id: &str,
    owner_only: bool,
) -> Result<String, Error> {
    let uid = auth::user(jar)?;
    let role = match kind {
        "document" => {
            crate::docs::get_document(State(state.clone()), Path(id.to_string()), jar.clone())
                .await
                .map_err(|_| (StatusCode::FORBIDDEN, "Document access denied".into()))?
                .0
                .effective_role
                .unwrap_or_default()
        }
        "project" => {
            crate::spaces::space_role(state, id, &Some(uid.clone()))
                .await
                .ok_or((StatusCode::FORBIDDEN, "Project access denied".into()))?
                .1
        }
        _ => return Err(bad("Unknown sync resource")),
    };
    if role != "owner" && (owner_only || role != "editor") {
        return Err((
            StatusCode::FORBIDDEN,
            "Editor access required; only the owner can configure a repository".into(),
        ));
    }
    Ok(uid)
}
async fn save_link(state: &AppState, kind: &str, id: &str, link: &Link) -> Result<(), Error> {
    sqlx::query("INSERT INTO github_links (kind,resource_id,target,head,base_hashes) VALUES (?,?,?,?,?) ON CONFLICT(kind,resource_id) DO UPDATE SET target=excluded.target,head=excluded.head,base_hashes=excluded.base_hashes")
        .bind(kind).bind(id).bind(serde_json::to_string(&link.target).unwrap()).bind(&link.head).bind(serde_json::to_string(&link.base_hashes).unwrap()).execute(&state.db).await.map_err(db_error)?;
    Ok(())
}

struct Local {
    files: Files,
    rows: Vec<(String, String, String)>,
}
// Prefer live CRDT state to the periodic database snapshot.
async fn current_text(state: &AppState, key: &str, blob: &[u8]) -> Vec<u8> {
    if let Some(group) = state.bcast_map.lock().await.get(key).cloned() {
        let awareness = group.awareness().read().await;
        let text = awareness.doc().get_or_insert_text("typst");
        return text.get_string(&awareness.doc().transact()).into_bytes();
    }
    crate::spaces::decode_text_blob(blob).into_bytes()
}
async fn local(state: &AppState, kind: &str, id: &str, target: &Target) -> Result<Local, Error> {
    let mut files = Files::new();
    let mut rows = Vec::new();
    if kind == "document" {
        let blob =
            sqlx::query_scalar::<_, Option<Vec<u8>>>("SELECT content FROM documents WHERE id=?")
                .bind(id)
                .fetch_one(&state.db)
                .await
                .map_err(db_error)?
                .unwrap_or_default();
        files.insert(target.source.clone(), current_text(state, id, &blob).await);
        for (fid, name, data) in sqlx::query_as::<_, (String, String, Vec<u8>)>(
            "SELECT id,name,data FROM files WHERE document_id=?",
        )
        .bind(id)
        .fetch_all(&state.db)
        .await
        .map_err(db_error)?
        {
            let path = target.repository_path(&name);
            if target.includes(&path) {
                if files.contains_key(&path) {
                    return Err(bad("An attachment duplicates another file or the linked source path. Rename it before syncing"));
                }
                files.insert(path.clone(), data);
                rows.push((fid, path, "binary".into()));
            }
        }
    } else {
        for (fid, path, kind, blob) in
            sqlx::query_as::<_, (String, String, String, Option<Vec<u8>>)>(
                "SELECT id,path,kind,content FROM space_files WHERE space_id=?",
            )
            .bind(id)
            .fetch_all(&state.db)
            .await
            .map_err(db_error)?
        {
            if !client::safe_path(&path) {
                return Err(bad("Project contains an invalid file path"));
            }
            let bytes = blob.unwrap_or_default();
            let data = if kind == "text" {
                current_text(state, &format!("space:{id}:{fid}"), &bytes).await
            } else {
                bytes
            };
            files.insert(path.clone(), data);
            rows.push((fid, path, kind));
        }
    }
    Ok(Local { files, rows })
}

pub async fn get_link(
    State(state): State<AppState>,
    Path((kind, id)): Path<(String, String)>,
    jar: SignedCookieJar,
) -> Result<Json<Value>, Error> {
    authorize(&state, &jar, &kind, &id, false).await?;
    Ok(Json(match link(&state, &kind, &id).await? {
        Some(link) => json!({"linked":true,"target":link.target,"head":link.head}),
        None => json!({"linked":false}),
    }))
}
pub async fn connect(
    State(state): State<AppState>,
    Path((kind, id)): Path<(String, String)>,
    jar: SignedCookieJar,
    Json(target): Json<Target>,
) -> Result<Json<Value>, Error> {
    let uid = authorize(&state, &jar, &kind, &id, true).await?;
    target
        .validate()
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    if (kind == "document") == target.source.is_empty() {
        return Err(bad(
            "Documents require a .typ file; projects connect to a whole repository",
        ));
    }
    let _gate = state.github.sync_gate.lock().await;
    let client = new_client(&state, auth::token(&state, &uid).await?);
    let remote_target = target.clone();
    let remote = tokio::task::spawn_blocking(move || client.snapshot(&remote_target))
        .await
        .map_err(|_| bad("GitHub request failed"))?
        .map_err(|e| (StatusCode::BAD_GATEWAY, e))?;
    if kind == "project"
        && !remote
            .files
            .contains_key(target.entrypoint.as_deref().unwrap_or("main.typ"))
    {
        return Err(bad("The project entrypoint does not exist on GitHub"));
    }
    let current = local(&state, &kind, &id, &target).await?;
    let mut baseline = hashes(&remote.files);
    baseline.retain(|path, _| current.files.contains_key(path));
    let link = Link {
        target,
        head: remote.head,
        base_hashes: baseline,
    };
    save_link(&state, &kind, &id, &link).await?;
    Ok(Json(
        json!({"linked":true,"target":link.target,"head":link.head}),
    ))
}
pub async fn disconnect(
    State(state): State<AppState>,
    Path((kind, id)): Path<(String, String)>,
    jar: SignedCookieJar,
) -> Result<StatusCode, Error> {
    authorize(&state, &jar, &kind, &id, true).await?;
    let _gate = state.github.sync_gate.lock().await;
    sqlx::query("DELETE FROM github_links WHERE kind=? AND resource_id=?")
        .bind(kind)
        .bind(id)
        .execute(&state.db)
        .await
        .map_err(db_error)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct SyncRequest {
    pub action: String,
    pub expected_local: Option<String>,
    pub expected_remote: Option<String>,
    #[serde(default)]
    pub discard_local: bool,
    pub message: Option<String>,
}
fn changes(base: &BTreeMap<String, String>, files: &Files) -> Vec<Value> {
    let current = hashes(files);
    let mut result = Vec::new();
    for (path, hash) in &current {
        if base.get(path) != Some(hash) {
            result.push(
                json!({"path":path,"change":if base.contains_key(path){"modified"}else{"added"}}),
            );
        }
    }
    for path in base.keys() {
        if !current.contains_key(path) {
            result.push(json!({"path":path,"change":"deleted"}));
        }
    }
    result
}
pub async fn sync(
    State(state): State<AppState>,
    Path((kind, id)): Path<(String, String)>,
    jar: SignedCookieJar,
    Json(payload): Json<SyncRequest>,
) -> Result<Json<Value>, Error> {
    let uid = authorize(&state, &jar, &kind, &id, false).await?;
    let _gate = state.github.sync_gate.lock().await;
    let mut link = link(&state, &kind, &id)
        .await?
        .ok_or(bad("Connect a GitHub repository first"))?;
    let client = new_client(&state, auth::token(&state, &uid).await?);
    let worker = client.clone();
    let target = link.target.clone();
    let remote = tokio::task::spawn_blocking(move || worker.snapshot(&target))
        .await
        .map_err(|_| bad("GitHub request failed"))?
        .map_err(|e| (StatusCode::BAD_GATEWAY, e))?;
    let current = local(&state, &kind, &id, &link.target).await?;
    let local_hash = fingerprint(&current.files);
    let local_changes = changes(&link.base_hashes, &current.files);
    if payload.action == "preview" {
        return Ok(Json(
            json!({"local_hash":local_hash,"remote_head":remote.head,"last_head":link.head,"local_changes":local_changes,"remote_changes":changes(&link.base_hashes,&remote.files),"files":remote.files.keys().collect::<Vec<_>>()}),
        ));
    }
    if payload.expected_local.as_deref() != Some(&local_hash)
        || payload.expected_remote.as_deref() != Some(&remote.head)
    {
        return Err(conflict(
            "Files changed after the preview. Review the changes again",
        ));
    }
    match payload.action.as_str() {
        "push" => {
            if remote.head != link.head {
                return Err(conflict(
                    "GitHub changed since the last sync. Pull and reconcile before pushing",
                ));
            }
            let target = link.target.clone();
            let expected = remote.head.clone();
            let files = current.files.clone();
            let mut previous = remote.files;
            retain_tracked_previous(&mut previous, &current.files, &link.base_hashes);
            let message = payload
                .message
                .unwrap_or_else(|| "Update Typst files from TypstDrive".into());
            if message.trim().is_empty() || message.len() > 1000 {
                return Err(bad("Enter a commit message under 1000 characters"));
            }
            link.head = tokio::task::spawn_blocking(move || {
                client.push(
                    &target,
                    &expected,
                    &previous,
                    &remote.modes,
                    &files,
                    &message,
                )
            })
            .await
            .map_err(|_| bad("GitHub push failed"))?
            .map_err(|e| (StatusCode::CONFLICT, e))?;
            link.base_hashes = hashes(&current.files);
        }
        "pull" => {
            if !local_changes.is_empty() && !payload.discard_local {
                return Err(conflict(
                    "There are local changes. Reconcile them, or explicitly choose to replace them",
                ));
            }
            if kind == "project"
                && !remote
                    .files
                    .contains_key(link.target.entrypoint.as_deref().unwrap_or("main.typ"))
            {
                return Err(bad("GitHub no longer contains the project entrypoint"));
            }
            apply_pull(
                &state,
                &kind,
                &id,
                &link.target,
                &current,
                &remote.files,
                &local_hash,
            )
            .await?;
            link.head = remote.head;
            link.base_hashes = hashes(&remote.files);
        }
        _ => return Err(bad("Unknown sync action")),
    }
    save_link(&state, &kind, &id, &link).await?;
    Ok(Json(
        json!({"head":link.head,"action":payload.action,"success":true}),
    ))
}

fn text_file(path: &str, bytes: &[u8]) -> bool {
    let extension = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    let binary = [
        "png", "jpg", "jpeg", "gif", "webp", "bmp", "ico", "avif", "tif", "tiff", "pdf", "ttf",
        "otf", "woff", "woff2", "eot", "zip", "tar", "gz", "bz2", "xz", "7z", "rar", "bin", "wasm",
        "exe", "dll",
    ];
    !binary.contains(&extension.as_str())
        && !bytes.contains(&0)
        && std::str::from_utf8(bytes).is_ok()
}
fn mime(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        _ => "application/octet-stream",
    }
}
fn replacement(blob: &[u8], source: &[u8]) -> Result<Vec<u8>, Error> {
    let source = std::str::from_utf8(source)
        .map_err(|_| bad("Typst and text dependencies must be UTF-8"))?;
    let doc = Doc::new();
    if let Ok(update) = Update::decode_v1(blob) {
        doc.transact_mut().apply_update(update);
    }
    let text = doc.get_or_insert_text("typst");
    {
        let mut txn = doc.transact_mut();
        let len = text.len(&txn);
        text.remove_range(&mut txn, 0, len);
        text.insert(&mut txn, 0, source);
    }
    let result = doc
        .transact()
        .encode_state_as_update_v1(&StateVector::default());
    Ok(result)
}

async fn apply_pull(
    state: &AppState,
    kind: &str,
    id: &str,
    target: &Target,
    current: &Local,
    files: &Files,
    expected: &str,
) -> Result<(), Error> {
    // Hold every existing CRDT write lock and the group registry through commit.
    // This prevents another editor or autosave from overwriting the pulled state.
    let groups = state.bcast_map.lock().await;
    let mut guards = BTreeMap::new();
    let keys: Vec<String> = if kind == "document" {
        vec![id.to_string()]
    } else {
        current
            .rows
            .iter()
            .filter(|r| r.2 == "text")
            .map(|r| format!("space:{id}:{}", r.0))
            .collect()
    };
    for key in keys {
        if let Some(group) = groups.get(&key) {
            guards.insert(key, group.awareness().clone().write_owned().await);
        }
    }
    let mut latest = current.files.clone();
    if kind == "document" {
        if let Some(a) = guards.get(id) {
            let t = a.doc().get_or_insert_text("typst");
            latest.insert(
                target.source.clone(),
                t.get_string(&a.doc().transact()).into_bytes(),
            );
        }
    } else {
        for (fid, path, _) in &current.rows {
            if let Some(a) = guards.get(&format!("space:{id}:{fid}")) {
                let t = a.doc().get_or_insert_text("typst");
                latest.insert(path.clone(), t.get_string(&a.doc().transact()).into_bytes());
            }
        }
    }
    if fingerprint(&latest) != expected {
        return Err(conflict(
            "An editor changed the document during pull. Review again",
        ));
    }
    let mut txn = state.db.begin().await.map_err(db_error)?;
    let mut updates = Vec::new();
    if kind == "document" {
        let source = files.get(&target.source).ok_or(bad(
            "The linked Typst file does not exist on GitHub yet. Push to create it first",
        ))?;
        let old = if let Some(a) = guards.get(id) {
            a.doc()
                .transact()
                .encode_state_as_update_v1(&StateVector::default())
        } else {
            sqlx::query_scalar::<_, Option<Vec<u8>>>("SELECT content FROM documents WHERE id=?")
                .bind(id)
                .fetch_one(&mut *txn)
                .await
                .map_err(db_error)?
                .unwrap_or_default()
        };
        let encoded = replacement(&old, source)?;
        sqlx::query("UPDATE documents SET content=?,updated_at=CURRENT_TIMESTAMP WHERE id=?")
            .bind(&encoded)
            .bind(id)
            .execute(&mut *txn)
            .await
            .map_err(db_error)?;
        updates.push((id.to_string(), encoded));
        for (fid, path, _) in &current.rows {
            if !files.contains_key(path) {
                sqlx::query("DELETE FROM files WHERE id=? AND document_id=?")
                    .bind(fid)
                    .bind(id)
                    .execute(&mut *txn)
                    .await
                    .map_err(db_error)?;
            }
        }
        let owner = sqlx::query_scalar::<_, String>("SELECT owner_id FROM documents WHERE id=?")
            .bind(id)
            .fetch_one(&mut *txn)
            .await
            .map_err(db_error)?;
        for (path, bytes) in files.iter().filter(|(p, _)| *p != &target.source) {
            let relative = target.relative_path(path);
            if let Some((fid, _, _)) = current.rows.iter().find(|r| &r.1 == path) {
                sqlx::query("UPDATE files SET data=?,mime_type=? WHERE id=?")
                    .bind(bytes)
                    .bind(mime(path))
                    .bind(fid)
                    .execute(&mut *txn)
                    .await
                    .map_err(db_error)?;
            } else {
                sqlx::query("INSERT INTO files (id,owner_id,document_id,name,mime_type,data) VALUES (?,?,?,?,?,?)").bind(uuid::Uuid::new_v4().to_string()).bind(&owner).bind(id).bind(relative).bind(mime(path)).bind(bytes).execute(&mut *txn).await.map_err(db_error)?;
            }
        }
    } else {
        sqlx::query("UPDATE spaces SET entrypoint=?,updated_at=CURRENT_TIMESTAMP WHERE id=?")
            .bind(target.entrypoint.as_deref().unwrap_or("main.typ"))
            .bind(id)
            .execute(&mut *txn)
            .await
            .map_err(db_error)?;
        for (fid, path, _) in &current.rows {
            if !files.contains_key(path) {
                sqlx::query("DELETE FROM space_files WHERE id=? AND space_id=?")
                    .bind(fid)
                    .bind(id)
                    .execute(&mut *txn)
                    .await
                    .map_err(db_error)?;
            }
        }
        for (path, bytes) in files {
            let previous = current.rows.iter().find(|r| &r.1 == path);
            let fid = previous
                .map(|r| r.0.clone())
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            let key = format!("space:{id}:{fid}");
            let is_text = text_file(path, bytes);
            if previous.map(|row| row.2.as_str()) == Some("text") && !is_text {
                return Err(bad("A local text file became binary on GitHub. Remove or rename the local file before pulling; no changes were applied"));
            }
            let encoded = if is_text {
                let old = if let Some(a) = guards.get(&key) {
                    a.doc()
                        .transact()
                        .encode_state_as_update_v1(&StateVector::default())
                } else {
                    sqlx::query_scalar::<_, Option<Vec<u8>>>(
                        "SELECT content FROM space_files WHERE id=?",
                    )
                    .bind(&fid)
                    .fetch_optional(&mut *txn)
                    .await
                    .map_err(db_error)?
                    .flatten()
                    .unwrap_or_default()
                };
                replacement(&old, bytes)?
            } else {
                bytes.clone()
            };
            if previous.is_some() {
                sqlx::query("UPDATE space_files SET kind=?,content=?,mime_type=? WHERE id=?")
                    .bind(if is_text { "text" } else { "binary" })
                    .bind(&encoded)
                    .bind(mime(path))
                    .bind(&fid)
                    .execute(&mut *txn)
                    .await
                    .map_err(db_error)?;
            } else {
                sqlx::query("INSERT INTO space_files (id,space_id,path,kind,content,mime_type) VALUES (?,?,?,?,?,?)").bind(&fid).bind(id).bind(path).bind(if is_text{"text"}else{"binary"}).bind(&encoded).bind(mime(path)).execute(&mut *txn).await.map_err(db_error)?;
            }
            if is_text {
                updates.push((key, encoded));
            }
        }
    }
    txn.commit().await.map_err(db_error)?;
    for (key, encoded) in updates {
        if let Some(a) = guards.get_mut(&key) {
            a.doc()
                .transact_mut()
                .apply_update(Update::decode_v1(&encoded).unwrap());
        }
    }
    drop(guards);
    drop(groups);
    Ok(())
}

#[derive(Deserialize)]
pub struct Import {
    pub target: Target,
    pub name: String,
    pub entrypoint: String,
}
pub async fn import(
    State(state): State<AppState>,
    jar: SignedCookieJar,
    Json(mut payload): Json<Import>,
) -> Result<Json<Value>, Error> {
    let uid = auth::user(&jar)?;
    payload
        .target
        .validate()
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    payload.target.entrypoint = Some(payload.entrypoint.clone());
    if !payload.target.source.is_empty()
        || !client::safe_path(&payload.entrypoint)
        || !payload.entrypoint.ends_with(".typ")
        || payload.name.trim().is_empty()
    {
        return Err(bad("Choose a repository, project name and .typ entrypoint"));
    }
    let _gate = state.github.sync_gate.lock().await;
    let client = new_client(&state, auth::token(&state, &uid).await?);
    let target = payload.target.clone();
    let remote = tokio::task::spawn_blocking(move || client.snapshot(&target))
        .await
        .map_err(|_| bad("GitHub import failed"))?
        .map_err(|e| (StatusCode::BAD_GATEWAY, e))?;
    if !remote.files.contains_key(&payload.entrypoint) {
        return Err(bad("Entrypoint does not exist in the repository"));
    }
    let id = uuid::Uuid::new_v4().to_string();
    let mut txn = state.db.begin().await.map_err(db_error)?;
    sqlx::query("INSERT INTO spaces (id,owner_id,name,entrypoint) VALUES (?,?,?,?)")
        .bind(&id)
        .bind(&uid)
        .bind(payload.name.trim())
        .bind(&payload.entrypoint)
        .execute(&mut *txn)
        .await
        .map_err(db_error)?;
    for (path, bytes) in &remote.files {
        let is_text = text_file(path, bytes);
        let data = if is_text {
            crate::spaces::encode_text_blob(std::str::from_utf8(bytes).unwrap())
        } else {
            bytes.clone()
        };
        sqlx::query("INSERT INTO space_files (id,space_id,path,kind,content,mime_type) VALUES (?,?,?,?,?,?)").bind(uuid::Uuid::new_v4().to_string()).bind(&id).bind(path).bind(if is_text{"text"}else{"binary"}).bind(data).bind(mime(path)).execute(&mut *txn).await.map_err(db_error)?;
    }
    let base = serde_json::to_string(&hashes(&remote.files)).unwrap();
    sqlx::query("INSERT INTO github_links (kind,resource_id,target,head,base_hashes) VALUES ('project',?,?,?,?)").bind(&id).bind(serde_json::to_string(&payload.target).unwrap()).bind(&remote.head).bind(base).execute(&mut *txn).await.map_err(db_error)?;
    txn.commit().await.map_err(db_error)?;
    Ok(Json(json!({"id":id,"head":remote.head})))
}

fn new_client(state: &AppState, token: String) -> client::Client {
    let client = client::Client::new(token);
    #[cfg(test)]
    if let Some(base) = &state.github.test_base {
        return client.with_test_base(base.clone());
    }
    #[cfg(not(test))]
    let _ = state;
    client
}

#[cfg(test)]
mod tests;
