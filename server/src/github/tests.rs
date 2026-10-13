use super::*;
use crate::compiler::TypstCompiler;
use axum_extra::extract::cookie::{Cookie, Key};
use std::sync::Arc;
use tokio::sync::RwLock;
use yrs::sync::Awareness;
use yrs_axum::broadcast::BroadcastGroup;

#[test]
fn first_push_preserves_remote_assets_not_yet_pulled() {
    let mut previous = Files::new();
    previous.insert("notes/main.typ".into(), b"old".to_vec());
    previous.insert("notes/images/remote.png".into(), vec![0, 255]);
    previous.insert(
        "notes/references.bib".into(),
        b"tracked bibliography".to_vec(),
    );
    let mut local = Files::new();
    local.insert("notes/main.typ".into(), b"new".to_vec());
    let baseline = hashes(&BTreeMap::from([(
        "notes/references.bib".into(),
        b"tracked bibliography".to_vec(),
    )]));
    retain_tracked_previous(&mut previous, &local, &baseline);
    assert!(previous.contains_key("notes/main.typ"));
    assert!(previous.contains_key("notes/references.bib"));
    assert!(!previous.contains_key("notes/images/remote.png"));
}

#[tokio::test]
async fn pull_cannot_change_a_live_text_file_to_binary_and_corrupt_autosave() {
    let state = state(None).await;
    sqlx::query("INSERT INTO spaces (id,owner_id,name,entrypoint) VALUES ('project','owner','Synthetic','main.typ')").execute(&state.db).await.unwrap();
    sqlx::query("INSERT INTO space_files (id,space_id,path,kind,content,mime_type) VALUES ('file','project','notes.txt','text',?,'text/plain')").bind(crate::spaces::encode_text_blob("local text")).execute(&state.db).await.unwrap();
    let mut target = target();
    target.source.clear();
    target.dependencies.clear();
    let current = local(&state, "project", "project", &target).await.unwrap();
    let doc = Doc::new();
    doc.get_or_insert_text("typst")
        .insert(&mut doc.transact_mut(), 0, "local text");
    let group = Arc::new(BroadcastGroup::new(Arc::new(RwLock::new(Awareness::new(doc))), 10).await);
    state
        .bcast_map
        .lock()
        .await
        .insert("space:project:file".into(), group);
    let mut remote = Files::new();
    remote.insert("notes.txt".into(), vec![0, 255, 0]);
    assert_eq!(
        apply_pull(
            &state,
            "project",
            "project",
            &target,
            &current,
            &remote,
            &fingerprint(&current.files)
        )
        .await
        .unwrap_err()
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        local(&state, "project", "project", &target)
            .await
            .unwrap()
            .files,
        current.files
    );
    assert!(text_file("LICENSE", b"Copyright example"));
    assert!(!text_file("font.ttf", b"synthetic font"));
}

async fn state(base: Option<String>) -> AppState {
    let db = crate::db::test_db().await;
    init_schema(&db).await;
    sqlx::query("INSERT INTO users (id,username,email,password_hash,is_admin) VALUES ('owner','owner','owner@example.invalid','synthetic',1),('viewer','viewer','viewer@example.invalid','synthetic',0)").execute(&db).await.unwrap();
    let state = AppState {
        github: Arc::new(Runtime {
            test_base: base,
            ..Runtime::default()
        }),
        compiler: Arc::new(Mutex::new(TypstCompiler::new())),
        access_revisions: Arc::new(Mutex::new(HashMap::new())),
        bcast_map: Arc::new(Mutex::new(HashMap::new())),
        db,
        key: Key::generate(),
        registration_enabled: false,
        rate_limiter: Arc::new(Mutex::new(HashMap::new())),
    };
    sqlx::query("INSERT INTO github_accounts (user_id,login,token,refresh_token,expires_at) VALUES ('owner','synthetic',?,'',?)").bind(auth::seal(&state,"synthetic-test-token")).bind(i64::MAX).execute(&state.db).await.unwrap();
    state
}
fn jar(state: &AppState, user: &str) -> SignedCookieJar {
    SignedCookieJar::new(state.key.clone()).add(Cookie::new("session_user_id", user.to_string()))
}
fn target() -> Target {
    Target {
        owner: "owner".into(),
        repository: "repo".into(),
        branch: "main".into(),
        source: "notes/main.typ".into(),
        dependencies: vec!["images/".into(), "references.bib".into()],
        entrypoint: None,
    }
}
async fn document(state: &AppState) -> String {
    crate::docs::create_document(
        State(state.clone()),
        jar(state, "owner"),
        Json(crate::models::CreateDocumentRequest {
            title: "Synthetic".into(),
            content: Some("old source".into()),
            folder_id: None,
        }),
    )
    .await
    .unwrap()
    .0
    .id
}
async fn live_document(state: &AppState, id: &str) {
    let blob = sqlx::query_scalar::<_, Vec<u8>>("SELECT content FROM documents WHERE id=?")
        .bind(id)
        .fetch_one(&state.db)
        .await
        .unwrap();
    let doc = Doc::new();
    doc.transact_mut()
        .apply_update(Update::decode_v1(&blob).unwrap());
    let awareness = Arc::new(RwLock::new(Awareness::new(doc)));
    let group = Arc::new(BroadcastGroup::new(awareness, 10).await);
    state.bcast_map.lock().await.insert(id.to_string(), group);
}
#[tokio::test]
async fn pull_preserves_binary_assets_bib_and_live_collaboration() {
    use axum::response::IntoResponse;
    use base64::Engine;
    let state = state(None).await;
    let id = document(&state).await;
    live_document(&state, &id).await;
    let target = target();
    let old = local(&state, "document", &id, &target).await.unwrap();
    let mut remote = Files::new();
    remote.insert(
        "notes/main.typ".into(),
        b"#image(\"images/plot.png\", width: 1cm)\n\nReference @example.\n\n#bibliography(\"references.bib\")".to_vec(),
    );
    remote.insert(
        "notes/images/plot.png".into(),
        base64::engine::general_purpose::STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==").unwrap(),
    );
    remote.insert(
        "notes/references.bib".into(),
        b"@article{example,title={Example},author={Doe, Jane},year={2026},journal={Testing}}"
            .to_vec(),
    );
    apply_pull(
        &state,
        "document",
        &id,
        &target,
        &old,
        &remote,
        &fingerprint(&old.files),
    )
    .await
    .unwrap();
    assert_eq!(
        local(&state, "document", &id, &target).await.unwrap().files,
        remote
    );
    let saved = sqlx::query_scalar::<_, Vec<u8>>("SELECT content FROM documents WHERE id=?")
        .bind(&id)
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert_eq!(
        crate::spaces::decode_text_blob(&saved).as_bytes(),
        remote["notes/main.typ"]
    );
    let response = crate::handlers::compile_handler(
        State(state.clone()),
        jar(&state, "owner"),
        Json(crate::handlers::CompileRequest {
            text: Some(String::from_utf8(remote["notes/main.typ"].clone()).unwrap()),
            document_id: Some(id),
            space_id: None,
            files: None,
        }),
    )
    .await
    .into_response();
    let body = axum::body::to_bytes(response.into_body(), 2 * 1024 * 1024)
        .await
        .unwrap();
    let compiled: Value = serde_json::from_slice(&body).unwrap();
    assert!(compiled["errors"].is_null(), "{}", compiled["errors"]);
    assert_eq!(compiled["svgs"].as_array().unwrap().len(), 1);
}
#[tokio::test]
async fn a_collaborative_edit_after_preview_blocks_pull() {
    let state = state(None).await;
    let id = document(&state).await;
    live_document(&state, &id).await;
    let target = target();
    let old = local(&state, "document", &id, &target).await.unwrap();
    let group = state.bcast_map.lock().await.get(&id).unwrap().clone();
    {
        let a = group.awareness().write().await;
        let t = a.doc().get_or_insert_text("typst");
        t.insert(&mut a.doc().transact_mut(), 0, "another editor: ");
    }
    let mut remote = Files::new();
    remote.insert(target.source.clone(), b"replacement".to_vec());
    assert_eq!(
        apply_pull(
            &state,
            "document",
            &id,
            &target,
            &old,
            &remote,
            &fingerprint(&old.files)
        )
        .await
        .unwrap_err()
        .0,
        StatusCode::CONFLICT
    );
    assert!(String::from_utf8(
        local(&state, "document", &id, &target).await.unwrap().files[&target.source].clone()
    )
    .unwrap()
    .contains("another editor"));
}
#[tokio::test]
async fn github_controls_reject_viewers_and_anonymous_users() {
    let state = state(None).await;
    let id = document(&state).await;
    sqlx::query("UPDATE documents SET public_role='viewer' WHERE id=?")
        .bind(&id)
        .execute(&state.db)
        .await
        .unwrap();
    assert_eq!(
        get_link(
            State(state.clone()),
            Path(("document".into(), id.clone())),
            SignedCookieJar::new(state.key.clone())
        )
        .await
        .unwrap_err()
        .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        get_link(
            State(state.clone()),
            Path(("document".into(), id.clone())),
            jar(&state, "viewer")
        )
        .await
        .unwrap_err()
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        connect(
            State(state.clone()),
            Path(("document".into(), id.clone())),
            jar(&state, "viewer"),
            Json(target())
        )
        .await
        .unwrap_err()
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        get_link(
            State(state.clone()),
            Path(("document".into(), id)),
            jar(&state, "owner")
        )
        .await
        .unwrap()
        .0["linked"],
        false
    );
}
#[tokio::test]
async fn disconnect_erases_encrypted_tokens_and_cancels_pending_connections() {
    let state = state(None).await;
    let cipher =
        sqlx::query_scalar::<_, String>("SELECT token FROM github_accounts WHERE user_id='owner'")
            .fetch_one(&state.db)
            .await
            .unwrap();
    assert!(!cipher.contains("synthetic-test-token"));
    assert_eq!(
        auth::token(&state, "owner").await.unwrap(),
        "synthetic-test-token"
    );
    let mut wrong_key = state.clone();
    wrong_key.key = Key::generate();
    assert_eq!(
        auth::token(&wrong_key, "owner").await.unwrap_err().0,
        StatusCode::UNAUTHORIZED
    );
    state.github.pending.lock().await.insert(
        "cancelled".into(),
        auth::Pending {
            user: "owner".into(),
            verifier: "synthetic".into(),
            expires: auth::now() + 60,
        },
    );
    auth::disconnect(State(state.clone()), jar(&state, "owner"))
        .await
        .unwrap();
    assert!(state.github.pending.lock().await.is_empty());
    assert_eq!(
        auth::token(&state, "owner").await.unwrap_err().0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        auth::callback(
            State(state.clone()),
            jar(&state, "owner"),
            axum::extract::Query(auth::Callback {
                code: Some("synthetic".into()),
                state: Some("cancelled".into())
            })
        )
        .await
        .unwrap_err()
        .0,
        StatusCode::BAD_REQUEST
    );
}
#[tokio::test]
async fn import_repository_keeps_paths_assets_and_bibliography() {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD;
    let (client, _, worker) = client::tests::fixture(vec![
        (200, json!({"object":{"sha":"head"}})),
        (200, json!({"tree":{"sha":"tree"}})),
        (
            200,
            json!({"truncated":false,"tree":[{"path":"main.typ","type":"blob","mode":"100644","size":5,"sha":"source"},{"path":"images/plot.png","type":"blob","mode":"100644","size":3,"sha":"image"},{"path":"refs/references.bib","type":"blob","mode":"100644","size":3,"sha":"bib"}]}),
        ),
        (
            200,
            json!({"encoding":"base64","content":b64.encode(b"hello")}),
        ),
        (
            200,
            json!({"encoding":"base64","content":b64.encode([0,255,1])}),
        ),
        (
            200,
            json!({"encoding":"base64","content":b64.encode(b"bib")}),
        ),
    ]);
    let state = state(Some(client.test_base())).await;
    let mut target = target();
    target.source.clear();
    target.dependencies.clear();
    let result = import(
        State(state.clone()),
        jar(&state, "owner"),
        Json(Import {
            target: target.clone(),
            name: "Synthetic project".into(),
            entrypoint: "main.typ".into(),
        }),
    )
    .await
    .unwrap()
    .0;
    worker.join().unwrap();
    let id = result["id"].as_str().unwrap();
    let files = local(&state, "project", id, &target).await.unwrap().files;
    assert_eq!(files.len(), 3);
    assert_eq!(files["images/plot.png"], vec![0, 255, 1]);
    assert_eq!(files["refs/references.bib"], b"bib");
    assert_eq!(files["main.typ"], b"hello");
    assert!(link(&state, "project", id).await.unwrap().is_some());
}
