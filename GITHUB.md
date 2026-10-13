# GitHub integration

Each TypstDrive user connects their own GitHub account. GitHub App user tokens
can access only repositories available to both that user and the installed app.
An editor never inherits the document owner's GitHub credentials.

## Documents

Use **GitHub** in the document toolbar to connect a repository, branch and
specific `.typ` file, existing or new. A first push can create a new file in an
existing repository branch. Add dependencies relative to that file's folder, one per
line. A trailing `/` selects a folder recursively, for example:

```text
images/
references.bib
styles/citation.csl
fonts/
```

Relative paths are preserved. Binary images and fonts stay binary. Bibliography
files are included like other dependencies. Files outside the selected source
and dependency paths are never changed. Dependencies outside the source file's
folder require a repository-backed project rather than `../` paths.

## Projects

Use **Import GitHub project** in the dashboard to import an existing repository
branch and select its `.typ` entrypoint. Existing spaces can also connect through
their GitHub toolbar button. A connected project syncs its complete repository
file tree, including text files, assets, bibliography files and included sources.
Existing unconnected spaces remain available until explicitly connected.

## Pull and push

Sync is manual. **Review changes** compares the current collaboration state,
the last sync baseline and the GitHub branch. Both sides' changed paths appear
before an action. Pulling over local changes requires an explicit checkbox.
Any edit after preview invalidates that preview. Pulled text is applied as a
CRDT update and broadcast to existing editors, with assets saved in the same
database transaction. Other editors may need to refresh the project file tree
after files are added or deleted.

**Commit & push** creates one Git commit for all changed files and deletions.
The branch update is never forced. A competing GitHub commit blocks the push;
there is no automatic merge or conflict-marker insertion. Reconcile conflicting
work before replacing local changes. New commits use GitHub's authenticated-user
attribution and preserve existing executable file modes.

Sync is limited to 1,000 files, 10 MiB per file, 100 MiB total, and an approximately
two-minute API-operation budget. Truncated GitHub trees, symlinks and submodules
are rejected. Sync does not run Git hooks or repository scripts.

## Administrator setup

Register a **GitHub App** with:

- Homepage: the production TypstDrive URL.
- Callback URL: `https://YOUR_HOST/api/github/callback`.
- Repository permission: **Contents: Read and write**; Metadata remains read-only.
- Installation availability: any account whose users should connect repositories.
- Webhooks disabled; this integration uses explicit pull/push.
- Expiring user tokens enabled. Authorization uses PKCE and one-time, expiring
  state bound to the signed-in TypstDrive account; refresh tokens stay server-side.

Configure the server environment:

```text
GITHUB_APP_SLUG=your-app-slug
GITHUB_CLIENT_ID=your-app-client-id
GITHUB_CLIENT_SECRET=your-app-client-secret
GITHUB_CALLBACK_URL=https://YOUR_HOST/api/github/callback
COOKIE_SECRET=a-stable-random-secret-of-at-least-64-bytes
```

Keep these values out of Git and logs. The stable cookie key also encrypts the
stored per-user GitHub access and refresh tokens. Changing it invalidates local
sessions and requires reconnecting GitHub. The UI remains unavailable until
configuration is present. Each user authorizes the app and uses **Choose
repositories** to install it on selected repositories. GitHub access does not
create a TypstDrive account or grant document editing rights.

Two additive database tables hold encrypted account connections and resource
links. Existing documents, spaces and their sharing permissions are unchanged.
Deleting a local account removes its stored GitHub connection; disconnecting
GitHub removes stored tokens while retaining repository links and local files.

## Verification

Rust tests use loopback GitHub API fixtures and synthetic SQLite data; they do
not need production credentials or modify real repositories. Tests cover path
isolation, image and bibliography integrity, atomic commits, branch races,
viewer restrictions, repository import and live collaborative pull conflicts.
Run `cargo test --locked --release -- --test-threads=1` from `server` in the
backend build environment. The existing sharing regression test remains in
`tests/permissions-smoke.mjs` and runs only against a disposable named instance.
`tests/github-permissions-smoke.mjs` checks the GitHub API privacy and setup
boundary, viewer restrictions, path validation and project deletion ownership.

Build the image using the instructions in `PERMISSIONS.md`, adding
`--build-arg SOURCE_REVISION=$(git rev-parse HEAD)` to the final Docker build.

API contracts follow GitHub's official documentation for
[GitHub App user authorization](https://docs.github.com/en/apps/creating-github-apps/authenticating-with-a-github-app/generating-a-user-access-token-for-a-github-app),
[Git trees](https://docs.github.com/en/rest/git/trees), and
[Git references](https://docs.github.com/en/rest/git/refs).
