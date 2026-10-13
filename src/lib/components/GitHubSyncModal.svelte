<script lang="ts">
    import { onMount } from 'svelte';
    import Icon from '@iconify/svelte';
    import { goto } from '$app/navigation';
    let { kind = 'document', resourceId = '', importing = false, onClose, onChanged = () => {} }:
        { kind?: 'document' | 'project'; resourceId?: string; importing?: boolean; onClose: () => void; onChanged?: () => void } = $props();
    let owner = $state('');
    let repository = $state('');
    let branch = $state('main');
    let source = $state('main.typ');
    let dependencies = $state('');
    let name = $state('');
    let entrypoint = $state('main.typ');
    let linked = $state(false);
    let configuring = $state(false);
    let account = $state<{configured:boolean;connected:boolean;login?:string;installation_url?:string} | null>(null);
    let canConfigure = $state(importing);
    let busy = $state(true);
    let error = $state('');
    let success = $state('');
    let message = $state('Update Typst files from TypstDrive');
    let discardLocal = $state(false);
    type Change = {path:string;change:string};
    let preview = $state<{local_hash:string;remote_head:string;last_head:string;local_changes:Change[];remote_changes:Change[];files:string[]} | null>(null);
    const endpoint = () => `/api/github/links/${kind}/${resourceId}`;
    async function request(url:string, method='GET', body?:unknown) {
        const response = await fetch(url,{method,headers:{'Content-Type':'application/json'},body:body===undefined?undefined:JSON.stringify(body)});
        if (!response.ok) throw new Error(await response.text());
        return response.status===204?null:response.json();
    }
    onMount(() => { (async () => {
        try {
            account = await request('/api/github/account');
            if (!importing) {
                const resource = await request(`/api/${kind==='document'?'docs':'spaces'}/${resourceId}`);
                canConfigure = resource.effective_role==='owner';
                if(kind==='project') entrypoint=resource.entrypoint;
                const data = await request(endpoint());
                linked = data.linked;
                if (linked) {
                    owner=data.target.owner;repository=data.target.repository;branch=data.target.branch;
                    source=data.target.source;dependencies=data.target.dependencies.join('\n');
                    if(data.target.entrypoint) entrypoint=data.target.entrypoint;
                }
            }
        } catch (e) { error=e instanceof Error?e.message:'Unable to load GitHub connection'; }
        busy=false;
    })(); });
    function target() { return {owner:owner.trim(),repository:repository.trim(),branch:branch.trim(),source:kind==='document'?source.trim():'',dependencies:kind==='document'?dependencies.split(/[\n,]/).map(s=>s.trim()).filter(Boolean):[],entrypoint:kind==='project'?entrypoint.trim():null}; }
    async function perform(action:'connect'|'preview'|'pull'|'push'|'import'|'disconnect') {
        busy=true;error='';success='';
        try {
            if(action==='import') {
                const result=await request('/api/github/import','POST',{target:target(),name:name.trim(),entrypoint:entrypoint.trim()});
                await goto(`/space/${result.id}`);onClose();
            } else if(action==='connect') {
                await request(endpoint(),'PUT',target());linked=true;configuring=false;preview=null;
                success='Repository connected. Review changes before pulling or pushing.';
            } else if(action==='disconnect') {
                await request(endpoint(),'DELETE');linked=false;preview=null;
            } else {
                const result=await request(endpoint()+'/sync','POST',{action,expected_local:preview?.local_hash,expected_remote:preview?.remote_head,discard_local:discardLocal,message});
                if(action==='preview'){preview=result;discardLocal=false;}
                else {preview=null;discardLocal=false;success=action==='pull'?'Pulled from GitHub.':'Committed and pushed to GitHub.';onChanged();}
            }
        } catch(e){error=e instanceof Error?e.message:'GitHub sync failed';if(action==='push'||action==='pull')preview=null;}
        busy=false;
    }
</script>

<div class="fixed inset-0 z-50 flex items-center justify-center bg-black/50 backdrop-blur-sm p-4">
    <section role="dialog" aria-modal="true" aria-labelledby="github-sync-title" class="w-full max-w-2xl max-h-[90vh] overflow-y-auto rounded-2xl border border-gray-200 dark:border-white/10 bg-white dark:bg-zinc-900 text-gray-900 dark:text-gray-100 shadow-2xl">
        <header class="flex items-center justify-between p-6 border-b border-gray-200 dark:border-white/10">
            <div class="flex items-center gap-3"><span class="flex h-10 w-10 items-center justify-center rounded-xl bg-gray-100 dark:bg-white/10"><Icon icon="mdi:github" class="text-2xl" /></span><div><h2 id="github-sync-title" class="text-lg font-semibold">{importing?'Import GitHub project':'GitHub sync'}</h2><p class="text-xs text-gray-500 dark:text-gray-400">{kind==='document'?'One document and its selected dependencies':'A repository and its complete file tree'}</p></div></div>
            <button onclick={onClose} disabled={busy} aria-label="Close GitHub sync" class="p-2 rounded-lg hover:bg-gray-100 dark:hover:bg-white/10"><Icon icon="mdi:close" class="text-xl" /></button>
        </header>
        <div class="p-6 space-y-5">
            {#if error}<p role="alert" class="rounded-lg p-3 text-sm bg-red-50 text-red-700 dark:bg-red-500/10 dark:text-red-300">{error}</p>{/if}
            {#if success}<p role="status" class="rounded-lg p-3 text-sm bg-green-50 text-green-700 dark:bg-green-500/10 dark:text-green-300">{success}</p>{/if}
            {#if busy}<p class="text-sm text-gray-500">Working with GitHub…</p>{/if}
            {#if account && !account.configured}<p class="text-sm text-gray-500">GitHub integration is awaiting server setup.</p>
            {:else if account && !account.connected}
                <p class="text-sm text-gray-500">Connect your own GitHub account. Repository access follows your GitHub permissions.</p>
                <a href="/api/github/connect" class="inline-flex items-center gap-2 rounded-lg bg-gray-900 dark:bg-white px-4 py-2 text-sm font-medium text-white dark:text-gray-900"><Icon icon="mdi:github" class="text-lg" />Connect GitHub</a>
            {:else if account}
                <div class="flex items-center justify-between gap-3"><p class="text-xs text-gray-500">Connected as <strong class="text-gray-800 dark:text-gray-200">{account.login}</strong></p>{#if account.installation_url}<a href={account.installation_url} target="_blank" rel="noopener noreferrer" class="text-xs text-gray-500 hover:underline">Choose repository access</a>{/if}</div>
                {#if importing || !linked || configuring}
                    {#if canConfigure}
                        <div class="grid grid-cols-2 gap-3">
                            <label class="text-sm">Repository owner<input bind:value={owner} placeholder="your-account" class="mt-1 w-full rounded-lg border-gray-300 bg-transparent dark:border-white/20" /></label>
                            <label class="text-sm">Repository<input bind:value={repository} placeholder="typst-notes" class="mt-1 w-full rounded-lg border-gray-300 bg-transparent dark:border-white/20" /></label>
                        </div>
                        <label class="block text-sm">Branch<input bind:value={branch} class="mt-1 w-full rounded-lg border-gray-300 bg-transparent dark:border-white/20" /></label>
                        {#if kind==='document'}
                            <label class="block text-sm">Typst file in the repository<input bind:value={source} placeholder="notes/main.typ" class="mt-1 w-full rounded-lg border-gray-300 bg-transparent dark:border-white/20" /></label>
                            <label class="block text-sm">Dependencies<textarea bind:value={dependencies} rows="3" placeholder="images/&#10;references.bib&#10;style.csl" class="mt-1 w-full rounded-lg border-gray-300 bg-transparent dark:border-white/20"></textarea><span class="block mt-1 text-xs text-gray-500">Paths are relative to the Typst file’s folder. One file or folder per line; end folder names with /. Images, bibliography files and fonts retain their paths. Other repository files stay untouched.</span></label>
                        {/if}
                        {#if kind==='project' && !importing}<label class="block text-sm">Entrypoint<input bind:value={entrypoint} class="mt-1 w-full rounded-lg border-gray-300 bg-transparent dark:border-white/20" /></label>{/if}
                        {#if importing}
                            <label class="block text-sm">Project name<input bind:value={name} class="mt-1 w-full rounded-lg border-gray-300 bg-transparent dark:border-white/20" /></label>
                            <label class="block text-sm">Entrypoint<input bind:value={entrypoint} class="mt-1 w-full rounded-lg border-gray-300 bg-transparent dark:border-white/20" /></label>
                        {/if}
                        <button disabled={busy || !owner || !repository || !branch || (importing && !name)} onclick={()=>perform(importing?'import':'connect')} class="rounded-lg bg-gray-900 dark:bg-white px-4 py-2 text-sm font-medium text-white dark:text-gray-900 disabled:opacity-40">{importing?'Import project':'Connect repository'}</button>
                    {:else}<p class="text-sm text-gray-500">The owner must connect this {kind} to a GitHub repository.</p>{/if}
                {:else}
                    <div class="rounded-xl border border-gray-200 dark:border-white/10 p-4"><p class="font-medium">{owner}/{repository}</p><p class="text-sm text-gray-500">{branch}{kind==='document'?` · ${source}`:''}</p></div>
                    <div class="flex gap-3 items-center"><button disabled={busy} onclick={()=>perform('preview')} class="rounded-lg bg-gray-900 dark:bg-white px-4 py-2 text-sm font-medium text-white dark:text-gray-900 disabled:opacity-40">Review changes</button>{#if canConfigure}<button disabled={busy} onclick={()=>{configuring=true;preview=null;}} class="text-sm text-gray-500 hover:underline">Change connection</button>{/if}</div>
                    {#if preview}
                        <div class="grid grid-cols-2 gap-4 text-sm">
                            <div><h3 class="font-semibold mb-2">Local changes</h3>{#each preview.local_changes as change}<p class="break-all text-xs py-1"><span class="text-gray-500">{change.change}</span> {change.path}</p>{:else}<p class="text-gray-500">No changes</p>{/each}</div>
                            <div><h3 class="font-semibold mb-2">GitHub changes</h3>{#each preview.remote_changes as change}<p class="break-all text-xs py-1"><span class="text-gray-500">{change.change}</span> {change.path}</p>{:else}<p class="text-gray-500">No changes</p>{/each}</div>
                        </div>
                        <details class="text-xs text-gray-500"><summary class="cursor-pointer">Files included ({preview.files.length})</summary>{#each preview.files as path}<p class="break-all py-1">{path}</p>{/each}</details>
                        {#if preview.remote_head!==preview.last_head}<p class="text-sm text-amber-700 dark:text-amber-300">The GitHub branch changed. Review and reconcile local changes before pulling. Push is blocked until the branch is synchronized.</p>{/if}
                        {#if preview.local_changes.length}<label class="flex items-start gap-2 text-sm"><input type="checkbox" bind:checked={discardLocal} class="mt-1 rounded" />Replace local changes with the GitHub version when pulling.</label>{/if}
                        <label class="block text-sm">Commit message<input bind:value={message} class="mt-1 w-full rounded-lg border-gray-300 bg-transparent dark:border-white/20" /></label>
                        <div class="flex gap-3"><button disabled={busy || (preview.local_changes.length>0&&!discardLocal)} onclick={()=>perform('pull')} class="flex items-center gap-2 rounded-lg border border-gray-300 dark:border-white/20 px-4 py-2 text-sm disabled:opacity-40"><Icon icon="mdi:download" />Pull from GitHub</button><button disabled={busy || preview.remote_head!==preview.last_head || !message.trim()} onclick={()=>perform('push')} class="flex items-center gap-2 rounded-lg bg-gray-900 dark:bg-white px-4 py-2 text-sm font-medium text-white dark:text-gray-900 disabled:opacity-40"><Icon icon="mdi:upload" />Commit & push</button></div>
                    {/if}
                {/if}
            {/if}
        </div>
    </section>
</div>
