use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    io::Read,
    time::{Duration, Instant},
};

pub const MAX_FILES: usize = 1000;
pub const MAX_FILE: usize = 10 * 1024 * 1024;
pub const MAX_TOTAL: usize = 100 * 1024 * 1024;
pub type Files = BTreeMap<String, Vec<u8>>;

#[derive(Clone, Deserialize, Serialize)]
pub struct Target {
    pub owner: String,
    pub repository: String,
    pub branch: String,
    // Repository path of the document; empty for a whole project.
    pub source: String,
    #[serde(default)]
    pub dependencies: Vec<String>,
    #[serde(default)]
    pub entrypoint: Option<String>,
}

pub fn safe_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 1024
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path.chars().any(char::is_control)
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != ".." && part != ".git")
}

pub fn url_segment(value: &str) -> String {
    value
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}
fn segment(value: &str) -> String {
    url_segment(value)
}

impl Target {
    pub fn validate(&self) -> Result<(), String> {
        let account = |s: &str| {
            !s.is_empty()
                && s.len() <= 100
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
                && s != "."
                && s != ".."
        };
        if !account(&self.owner)
            || !account(&self.repository)
            || !safe_path(&self.branch)
            || (!self.source.is_empty()
                && (!safe_path(&self.source) || !self.source.ends_with(".typ")))
            || self.dependencies.len() > MAX_FILES
            || self
                .entrypoint
                .as_ref()
                .map(|s| !safe_path(s) || !s.ends_with(".typ"))
                .unwrap_or(false)
            || self
                .dependencies
                .iter()
                .any(|s| !safe_path(s.trim_end_matches('/')))
        {
            return Err(
                "Use a valid GitHub repository, branch, .typ file, and relative dependency paths"
                    .into(),
            );
        }
        Ok(())
    }

    pub fn prefix(&self) -> &str {
        self.source
            .rsplit_once('/')
            .map(|(parent, _)| parent)
            .unwrap_or("")
    }

    pub fn includes(&self, path: &str) -> bool {
        if self.source.is_empty() {
            return safe_path(path);
        }
        if path == self.source {
            return true;
        }
        self.dependencies.iter().any(|relative| {
            let full = if self.prefix().is_empty() {
                relative.clone()
            } else {
                format!("{}/{relative}", self.prefix())
            };
            path == full || (full.ends_with('/') && path.starts_with(&full))
        })
    }

    pub fn repository_path(&self, relative: &str) -> String {
        if self.prefix().is_empty() {
            relative.to_string()
        } else {
            format!("{}/{relative}", self.prefix())
        }
    }

    pub fn relative_path<'a>(&self, path: &'a str) -> &'a str {
        if self.prefix().is_empty() {
            path
        } else {
            &path[self.prefix().len() + 1..]
        }
    }
}

#[derive(Clone)]
pub struct Client {
    agent: ureq::Agent,
    base: String,
    token: String,
    started: Instant,
}
pub struct Snapshot {
    pub head: String,
    pub modes: BTreeMap<String, String>,
    pub files: Files,
}

impl Client {
    #[cfg(test)]
    pub(crate) fn with_test_base(mut self, base: String) -> Self {
        self.base = base;
        self
    }
    #[cfg(test)]
    pub(crate) fn test_base(&self) -> String {
        self.base.clone()
    }
    pub fn new(token: String) -> Self {
        Self {
            agent: ureq::AgentBuilder::new()
                .timeout(Duration::from_secs(30))
                .redirects(0)
                .build(),
            base: "https://api.github.com".into(),
            token,
            started: Instant::now(),
        }
    }

    pub fn request(&self, method: &str, path: &str, body: Option<Value>) -> Result<Value, String> {
        if self.started.elapsed() > Duration::from_secs(120) {
            return Err("GitHub sync took too long; reduce the selected files and retry".into());
        }
        let request = self
            .agent
            .request(method, &format!("{}{path}", self.base))
            .set("Authorization", &format!("Bearer {}", self.token))
            .set("User-Agent", "TypstDrive-GitHub")
            .set("Accept", "application/vnd.github+json")
            .set("X-GitHub-Api-Version", "2022-11-28");
        let response = match body {
            Some(body) => request
                .set("Content-Type", "application/json")
                .send_string(&body.to_string()),
            None => request.call(),
        }
        .map_err(|e| match e {
            ureq::Error::Status(code, _) => format!(
                "GitHub returned HTTP {code}; check repository access or refresh your connection"
            ),
            _ => "GitHub request failed; retry later".into(),
        })?;
        let mut data = Vec::new();
        response
            .into_reader()
            .take((MAX_FILE * 2 + 1) as u64)
            .read_to_end(&mut data)
            .map_err(|_| "Cannot read GitHub response")?;
        if data.len() > MAX_FILE * 2 {
            return Err("GitHub response exceeds the size limit".into());
        }
        serde_json::from_slice(&data).map_err(|_| "Invalid GitHub response".into())
    }

    fn repo(target: &Target) -> String {
        format!(
            "/repos/{}/{}",
            segment(&target.owner),
            segment(&target.repository)
        )
    }

    pub fn head(&self, target: &Target) -> Result<String, String> {
        let value = self.request(
            "GET",
            &format!(
                "{}/git/ref/heads/{}",
                Self::repo(target),
                segment(&target.branch)
            ),
            None,
        )?;
        value["object"]["sha"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| "GitHub branch not found".into())
    }

    pub fn snapshot(&self, target: &Target) -> Result<Snapshot, String> {
        target.validate()?;
        let head = self.head(target)?;
        let commit = self.request(
            "GET",
            &format!("{}/git/commits/{head}", Self::repo(target)),
            None,
        )?;
        let tree = commit["tree"]["sha"]
            .as_str()
            .ok_or("GitHub commit has no tree")?
            .to_string();
        let listing = self.request(
            "GET",
            &format!("{}/git/trees/{tree}?recursive=1", Self::repo(target)),
            None,
        )?;
        if listing["truncated"].as_bool().unwrap_or(true) {
            return Err("Repository tree is too large to sync safely".into());
        }
        let mut files = Files::new();
        let mut modes = BTreeMap::new();
        let mut total = 0;
        for entry in listing["tree"].as_array().ok_or("GitHub tree is missing")? {
            let path = entry["path"].as_str().ok_or("Invalid GitHub path")?;
            if target.source.is_empty() && !safe_path(path) {
                return Err("Repository contains an unsupported file path".into());
            }
            if !target.includes(path) || entry["type"] == "tree" {
                continue;
            }
            if entry["type"] != "blob" || (entry["mode"] != "100644" && entry["mode"] != "100755") {
                return Err(format!("Cannot sync symlink or submodule: {path}"));
            }
            let size = entry["size"].as_u64().ok_or("GitHub file size missing")? as usize;
            total += size;
            if size > MAX_FILE || total > MAX_TOTAL || files.len() >= MAX_FILES {
                return Err("Repository exceeds sync size limits".into());
            }
            let sha = entry["sha"].as_str().ok_or("GitHub blob missing")?;
            let blob = self.request(
                "GET",
                &format!("{}/git/blobs/{sha}", Self::repo(target)),
                None,
            )?;
            if blob["encoding"] != "base64" {
                return Err("Unsupported GitHub blob encoding".into());
            }
            let content: String = blob["content"]
                .as_str()
                .ok_or("GitHub blob content missing")?
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect();
            let bytes = STANDARD
                .decode(content)
                .map_err(|_| "Invalid GitHub blob")?;
            if bytes.len() != size {
                return Err("GitHub blob size mismatch".into());
            }
            files.insert(path.to_string(), bytes);
            modes.insert(
                path.to_string(),
                entry["mode"].as_str().unwrap().to_string(),
            );
        }
        Ok(Snapshot { head, modes, files })
    }

    // All files, assets and deletions become one commit. Never force-update refs.
    pub fn push(
        &self,
        target: &Target,
        expected_head: &str,
        previous: &Files,
        modes: &BTreeMap<String, String>,
        files: &Files,
        message: &str,
    ) -> Result<String, String> {
        target.validate()?;
        if self.head(target)? != expected_head {
            return Err(
                "Conflict: GitHub changed since the last sync. Pull and reconcile first".into(),
            );
        }
        let commit = self.request(
            "GET",
            &format!("{}/git/commits/{expected_head}", Self::repo(target)),
            None,
        )?;
        let base_tree = commit["tree"]["sha"]
            .as_str()
            .ok_or("GitHub tree missing")?;
        let mut changes = Vec::new();
        let mut total = 0;
        for (path, content) in files {
            if !target.includes(path) || !safe_path(path) {
                return Err("File is outside the linked sync paths".into());
            }
            total += content.len();
            if content.len() > MAX_FILE || total > MAX_TOTAL || files.len() > MAX_FILES {
                return Err("Files exceed sync size limits".into());
            }
            if previous.get(path) == Some(content) {
                continue;
            }
            let blob = self.request(
                "POST",
                &format!("{}/git/blobs", Self::repo(target)),
                Some(json!({"encoding":"base64","content":STANDARD.encode(content)})),
            )?;
            changes.push(json!({"path":path,"mode":modes.get(path).map(String::as_str).unwrap_or("100644"),"type":"blob","sha":blob["sha"]}));
        }
        for path in previous.keys().filter(|path| !files.contains_key(*path)) {
            if !target.includes(path) {
                return Err("Deletion is outside the linked sync paths".into());
            }
            changes.push(json!({"path":path,"mode":"100644","type":"blob","sha":null}));
        }
        if changes.is_empty() {
            return Ok(expected_head.to_string());
        }
        let tree = self.request(
            "POST",
            &format!("{}/git/trees", Self::repo(target)),
            Some(json!({"base_tree":base_tree,"tree":changes})),
        )?;
        let new_commit = self.request(
            "POST",
            &format!("{}/git/commits", Self::repo(target)),
            Some(json!({"message":message,"tree":tree["sha"],"parents":[expected_head]})),
        )?;
        let sha = new_commit["sha"]
            .as_str()
            .ok_or("GitHub commit creation failed")?;
        // A concurrent branch update makes this commit non-fast-forward and fails.
        self.request(
            "PATCH",
            &format!(
                "{}/git/refs/heads/{}",
                Self::repo(target),
                segment(&target.branch)
            ),
            Some(json!({"sha":sha,"force":false})),
        )?;
        Ok(sha.to_string())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::{
        io::Write,
        net::TcpListener,
        sync::{Arc, Mutex},
    };
    pub(crate) fn fixture(
        responses: Vec<(u16, Value)>,
    ) -> (
        Client,
        Arc<Mutex<Vec<(String, Value)>>>,
        std::thread::JoinHandle<()>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let log = Arc::new(Mutex::new(Vec::new()));
        let received = log.clone();
        let thread = std::thread::spawn(move || {
            for (status, value) in responses {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut data = Vec::new();
                let mut byte = [0];
                while !data.ends_with(b"\r\n\r\n") {
                    stream.read_exact(&mut byte).unwrap();
                    data.push(byte[0]);
                }
                let header = String::from_utf8(data).unwrap();
                let line = header.lines().next().unwrap().to_string();
                let len = header
                    .lines()
                    .find_map(|s| {
                        s.to_lowercase()
                            .strip_prefix("content-length:")
                            .and_then(|v| v.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                let mut body = vec![0; len];
                stream.read_exact(&mut body).unwrap();
                received.lock().unwrap().push((
                    line,
                    if body.is_empty() {
                        Value::Null
                    } else {
                        serde_json::from_slice(&body).unwrap()
                    },
                ));
                let body = value.to_string();
                write!(stream,"HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
            }
        });
        let mut client = Client::new("synthetic-test-token".into());
        client.base = format!("http://{address}");
        (client, log, thread)
    }
    fn document() -> Target {
        Target {
            owner: "owner".into(),
            repository: "repo".into(),
            branch: "main".into(),
            source: "notes/main.typ".into(),
            dependencies: vec!["images/".into(), "references.bib".into()],
            entrypoint: None,
        }
    }
    #[test]
    fn snapshot_preserves_binary_images_and_bibliographies() {
        let bytes = vec![0, 255, 137, 80, 78, 71];
        let target = document();
        let (client, log, worker) = fixture(vec![
            (200, json!({"object":{"sha":"head"}})),
            (200, json!({"tree":{"sha":"tree"}})),
            (
                200,
                json!({"truncated":false,"tree":[
                {"path":"notes/main.typ","type":"blob","mode":"100644","size":5,"sha":"source"},
                {"path":"notes/images/plot.png","type":"blob","mode":"100644","size":6,"sha":"image"},
                {"path":"notes/references.bib","type":"blob","mode":"100644","size":3,"sha":"bib"},
                {"path":"unrelated.txt","type":"blob","mode":"100644","size":100,"sha":"other"}]}),
            ),
            (
                200,
                json!({"encoding":"base64","content":STANDARD.encode(b"hello")}),
            ),
            (
                200,
                json!({"encoding":"base64","content":STANDARD.encode(&bytes)}),
            ),
            (
                200,
                json!({"encoding":"base64","content":STANDARD.encode(b"bib")}),
            ),
        ]);
        let result = client.snapshot(&target).unwrap();
        worker.join().unwrap();
        assert_eq!(result.files.len(), 3);
        assert_eq!(result.files["notes/images/plot.png"], bytes);
        assert_eq!(result.files["notes/references.bib"], b"bib");
        assert!(!log
            .lock()
            .unwrap()
            .iter()
            .any(|(line, _)| line.contains("other")));
    }
    #[test]
    fn push_commits_source_assets_and_deletions_together_without_force() {
        let target = document();
        let mut old = Files::new();
        old.insert("notes/main.typ".into(), b"old".to_vec());
        old.insert("notes/images/old.png".into(), vec![1]);
        let mut new = Files::new();
        new.insert("notes/main.typ".into(), b"new".to_vec());
        new.insert("notes/references.bib".into(), b"bib".to_vec());
        let (client, log, worker) = fixture(vec![
            (200, json!({"object":{"sha":"head"}})),
            (200, json!({"tree":{"sha":"tree"}})),
            (201, json!({"sha":"source-blob"})),
            (201, json!({"sha":"bib-blob"})),
            (201, json!({"sha":"new-tree"})),
            (201, json!({"sha":"new-head"})),
            (200, json!({})),
        ]);
        assert_eq!(
            client
                .push(
                    &target,
                    "head",
                    &old,
                    &BTreeMap::new(),
                    &new,
                    "Update notes"
                )
                .unwrap(),
            "new-head"
        );
        worker.join().unwrap();
        let requests = log.lock().unwrap();
        let tree = &requests[4].1;
        assert_eq!(tree["base_tree"], "tree");
        assert_eq!(tree["tree"].as_array().unwrap().len(), 3);
        assert!(tree["tree"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["path"] == "notes/images/old.png" && v["sha"].is_null()));
        assert_eq!(requests[5].1["parents"], json!(["head"]));
        assert_eq!(requests[6].1["force"], false);
    }
    #[test]
    fn advancing_remote_branch_blocks_push_before_creating_blobs() {
        let (client, log, worker) = fixture(vec![(200, json!({"object":{"sha":"someone-else"}}))]);
        assert!(client
            .push(
                &document(),
                "head",
                &Files::new(),
                &BTreeMap::new(),
                &Files::new(),
                "Update"
            )
            .unwrap_err()
            .contains("Conflict"));
        worker.join().unwrap();
        assert_eq!(log.lock().unwrap().len(), 1);
    }
    #[test]
    fn a_new_document_path_can_connect_before_its_first_push() {
        let (client, _, worker) = fixture(vec![
            (200, json!({"object":{"sha":"head"}})),
            (200, json!({"tree":{"sha":"tree"}})),
            (200, json!({"truncated":false,"tree":[]})),
        ]);
        let snapshot = client.snapshot(&document()).unwrap();
        worker.join().unwrap();
        assert!(snapshot.files.is_empty());
        assert_eq!(snapshot.head, "head");
    }
    #[test]
    fn branch_race_does_not_force_overwrite_the_remote() {
        let mut files = Files::new();
        files.insert("notes/main.typ".into(), b"new".to_vec());
        let (client, log, worker) = fixture(vec![
            (200, json!({"object":{"sha":"head"}})),
            (200, json!({"tree":{"sha":"tree"}})),
            (201, json!({"sha":"blob"})),
            (201, json!({"sha":"new-tree"})),
            (201, json!({"sha":"new-head"})),
            (422, json!({"message":"not a fast-forward"})),
        ]);
        assert!(client
            .push(
                &document(),
                "head",
                &Files::new(),
                &BTreeMap::new(),
                &files,
                "Update"
            )
            .is_err());
        worker.join().unwrap();
        assert_eq!(log.lock().unwrap().last().unwrap().1["force"], false);
    }
    #[test]
    fn document_paths_preserve_dependencies_and_exclude_neighbors() {
        let target = Target {
            owner: "owner".into(),
            repository: "repo".into(),
            branch: "feature/notes".into(),
            source: "notes/main.typ".into(),
            dependencies: vec!["images/".into(), "references.bib".into()],
            entrypoint: None,
        };
        assert!(target.validate().is_ok());
        for path in [
            "notes/main.typ",
            "notes/images/plot.png",
            "notes/references.bib",
        ] {
            assert!(target.includes(path));
        }
        for path in [
            "main.typ",
            "notes/other.typ",
            "notes/images-other/plot.png",
            "secrets.txt",
        ] {
            assert!(!target.includes(path));
        }
        assert_eq!(
            target.relative_path("notes/images/plot.png"),
            "images/plot.png"
        );
        assert_eq!(
            target.repository_path("references.bib"),
            "notes/references.bib"
        );
        for path in ["../secret", "/abs", "a//b", "a/../b", "a\\b", ".git/config"] {
            assert!(!safe_path(path));
        }
    }
}
