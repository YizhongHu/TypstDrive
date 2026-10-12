import assert from 'node:assert/strict';
import * as Y from 'yjs';
import * as sync from 'y-protocols/sync';
import * as encoding from 'lib0/encoding';
import * as decoding from 'lib0/decoding';

// Only run against the isolated test container, never production.
const base = process.env.TEST_URL;
assert(base && base.includes('typst-permissions-test'), 'Isolated test URL required');
const request = async (path, cookie = '', method = 'GET', data) => fetch(base + path, {
  method, headers: { Cookie: cookie, 'Content-Type': 'application/json' },
  body: data === undefined ? undefined : JSON.stringify(data)
});
const password = 'disposable-local-test-credential';
let res = await request('/api/setup', '', 'POST', {username:'owner',email:'owner@example.invalid',password});
assert.equal(res.status, 200);
const owner = res.headers.get('set-cookie').split(';')[0];
const users = {};
for (const name of ['editor','outsider']) {
  res = await request('/api/admin/users', owner, 'POST', {username:name,email:`${name}@example.invalid`,password,is_admin:false});
  assert.equal(res.status, 201); users[name] = await res.json();
}
const login = async name => {
  const r = await request('/api/auth/login', '', 'POST', {email:`${name}@example.invalid`,password});
  assert.equal(r.status, 200); return r.headers.get('set-cookie').split(';')[0];
};
const editor = await login('editor'), outsider = await login('outsider');
assert.equal((await request('/api/admin/users', editor)).status,403);
assert.equal((await request('/api/auth/register','', 'POST',{username:'unapproved',email:'unapproved@example.invalid',password})).status,403);
const unscopedCompile = await (await request('/api/compile', '', 'POST', {text:'hello'})).json();
assert.equal(unscopedCompile.svgs, null);
assert.equal(unscopedCompile.errors[0].message, 'Unauthorized');
res = await request('/api/docs', owner, 'POST', {title:'Permission test',content:'= Shared example\nInitial text'});
assert.equal(res.status, 200); const document = await res.json(), id = document.id;
assert.equal((await request(`/api/docs/${id}`)).status, 401);
assert.equal((await request(`/api/docs/${id}`, outsider)).status, 401);
for (const endpoint of ['comments','versions']) {
  assert.equal((await request(`/api/docs/${id}/${endpoint}`, outsider)).status, 401);
}
assert.equal((await request(`/api/docs/${id}/comments`, outsider, 'POST', {content:'unauthorized'})).status, 401);
assert.equal((await request(`/api/docs/${id}`, owner, 'PATCH', {public_role:'editor'})).status, 400);
const rejectedSocket = new WebSocket(base.replace('http:', 'ws:') + `/yjs/${id}`);
await Promise.race([
  new Promise((resolve, reject) => {
    rejectedSocket.onerror = resolve;
    rejectedSocket.onopen = () => reject(new Error('Anonymous private websocket opened'));
  }), pauseBefore(2000)
]);
function pauseBefore(ms) { return new Promise((_,reject)=>setTimeout(()=>reject(new Error('Handshake rejection timed out')),ms)); }
res = await request(`/api/docs/${id}`, owner, 'PATCH', {public_role:'viewer'}); assert.equal(res.status, 200);
assert.equal((await (await request(`/api/docs/${id}`)).json()).effective_role, 'viewer');
assert.equal((await request(`/api/docs/${id}`, '', 'PATCH', {title:'unauthorized'})).status, 401);
assert.equal((await request(`/api/docs/${id}/comments`, outsider, 'POST', {content:'unauthorized'})).status, 403);
res = await request(`/api/docs/${id}/invite`, owner, 'POST', {email:'editor@example.invalid',role:'editor'});
assert.equal(res.status, 200);
assert.equal((await (await request(`/api/docs/${id}`, editor)).json()).effective_role, 'editor');

function connect(cookie) {
  const doc = new Y.Doc();
  const ws = new WebSocket(base.replace('http:', 'ws:') + `/yjs/${id}`, {headers:{Cookie:cookie}});
  ws.binaryType = 'arraybuffer';
  ws.onmessage = event => {
    const decoder = decoding.createDecoder(new Uint8Array(event.data));
    while (decoding.hasContent(decoder)) {
      const type = decoding.readVarUint(decoder);
      if (type !== 0) break;
      const encoder = encoding.createEncoder(); encoding.writeVarUint(encoder, 0);
      sync.readSyncMessage(decoder, encoder, doc, 'remote');
      if (encoding.length(encoder) > 1) ws.send(encoding.toUint8Array(encoder));
    }
  };
  const opened = new Promise((resolve,reject) => {
    ws.onopen = () => {
      const encoder = encoding.createEncoder(); encoding.writeVarUint(encoder,0);
      sync.writeSyncStep1(encoder,doc); ws.send(encoding.toUint8Array(encoder)); resolve();
    };
    ws.onerror=reject;
  });
  return {doc,ws,opened};
}
const pause = ms => new Promise(resolve=>setTimeout(resolve,ms));
const viewer = connect(''), writer = connect(editor); await Promise.all([viewer.opened,writer.opened]); await pause(500);
assert(viewer.doc.getText('typst').toString().includes('Initial text'));
const fake = new Y.Doc(); fake.getText('typst').insert(0, 'VIEWER MUST NOT WRITE');
const mutation = encoding.createEncoder(); encoding.writeVarUint(mutation,0);sync.writeUpdate(mutation,Y.encodeStateAsUpdate(fake));
viewer.ws.send(encoding.toUint8Array(mutation));
// A valid state-vector request followed by an update used to bypass the filter.
const bundled = encoding.createEncoder();encoding.writeVarUint(bundled,0);sync.writeSyncStep1(bundled,viewer.doc);
encoding.writeVarUint(bundled,0);sync.writeUpdate(bundled,Y.encodeStateAsUpdate(fake));viewer.ws.send(encoding.toUint8Array(bundled));
writer.doc.getText('typst').insert(writer.doc.getText('typst').length, '\nApproved editor change');
const update = encoding.createEncoder();encoding.writeVarUint(update,0);sync.writeUpdate(update,Y.encodeStateAsUpdate(writer.doc));writer.ws.send(encoding.toUint8Array(update));
await pause(6500);
assert(viewer.doc.getText('typst').toString().includes('Approved editor change'));
const deviceRes = await request('/api/desktop/auth/login', '', 'POST', {email:'owner@example.invalid', password});
assert.equal(deviceRes.status,200); const {token} = await deviceRes.json();
const savedRes = await fetch(base+`/api/desktop/documents/${id}`, {headers:{Authorization:`Bearer ${token}`}});
assert.equal(savedRes.status,200); const saved = await savedRes.json();
assert(saved.content.includes('Approved editor change'));
assert(!saved.content.includes('VIEWER MUST NOT WRITE'));
res=await request('/api/compile','', 'POST',{document_id:id,text:'#this-must-not-compile'});
const compiled=await res.json();assert.equal(compiled.errors,null);assert.equal(compiled.svgs.length,1);
const revoked = new Promise(resolve => { viewer.ws.onclose = resolve; });
res=await request(`/api/docs/${id}`,owner,'PATCH',{public_role:'none'});assert.equal(res.status,200);
assert.equal((await request(`/api/docs/${id}`)).status,401);
await Promise.race([revoked, pause(2000).then(()=>{throw new Error('Viewer was not disconnected after revocation');})]);
writer.ws.close();
assert.equal((await request(`/api/admin/users/${users.outsider.id}`,owner,'DELETE')).status,204);
assert.equal((await request('/api/docs',outsider,'POST',{title:'Revoked account',content:'blocked'})).status,401);
const removedEditor = connect(editor); await removedEditor.opened;
const accountRevoked = new Promise(resolve => { removedEditor.ws.onclose = resolve; });
assert.equal((await request(`/api/admin/users/${users.editor.id}`,owner,'DELETE')).status,204);
await Promise.race([accountRevoked, pause(2000).then(()=>{throw new Error('Removed account retained an open editing socket');})]);
console.log('PASS: private reads, account roles, public viewer, rejected viewer writes, editor sync/persistence, and saved-source compilation');
