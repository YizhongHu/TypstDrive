# Production sharing changes

Base: upstream commit `5d3681bd3f3016275ef829445b75c239761495ea`, matching
the deployed image digest `9ea2a2e9b7c4b6943354b6a1a8ed6c83e70ef14048195f46e45758df30429a87`.

Accounts are the editor whitelist: public registration stays disabled and only
the administrator creates accounts. Each document additionally requires an
owner or explicit Editor collaborator role for editing. Public links grant
Viewer access only, including legacy links formerly configured as public Editor.

Changes:

- Reject private document and Space WebSocket connections before upgrading.
- Permit only a single sync-state request or awareness message from viewers;
  reject state updates and appended update messages.
- Close active connections on sharing or collaborator changes; clients must
  reconnect and pass current permission checks.
- Enforce document access on comments, history, compilation and export; require
  editor access for LSP. Viewers compile/export saved content, not arbitrary
  submitted source. Public preview refreshes every six seconds to follow saved
  edits (the upstream persistence interval is five seconds).
- Share dialog now persists Restricted/Viewer access before copying the URL.
  The URL does not grant permissions by itself.

Build locally with Docker (no production data mounted):

```sh
docker build --platform linux/amd64 --target backend-builder -t typstdrive-permissions-build .
docker build --platform linux/amd64 --target frontend-builder -t typstdrive-permissions-frontend .
docker build --platform linux/amd64 -f Dockerfile.permissions -t typstdrive-production:permissions .
```

The final image reuses the official pinned runtime, replacing only the backend
and frontend. Dependency locks from the tested build are committed, and the
build uses frozen Bun and locked Cargo dependencies.

Run the HTTP/WebSocket smoke test only against a disposable container/database
named `typst-permissions-test`; `tests/permissions-smoke.mjs` refuses other URLs.
Its credentials and documents are synthetic. The test includes private reads,
viewer write attacks, editor synchronization, persistence, and revocation.

Production rollout must preserve its existing external data volume. No database
schema changes are included. Keep Cloudflare Access until isolated tests pass,
then remove only the Typst Access application after required confirmation.
Cloudflare Tunnel and its HTTPS hostname remain in use.

Current session-cookie key is generated at startup, so a restart requires users
to sign in again. A stable cookie-signing secret has not been configured.

## Verification

The isolated HTTP/WebSocket smoke test passed, including private reads,
viewer update and bundled-message attacks, approved editor persistence,
sharing revocation and account removal. Browser verification confirmed an
anonymous rendered preview with a Read-only label, zoom/export controls
and no editing controls. Production rollout retained the existing database
and data volume; database integrity returned `ok`.
