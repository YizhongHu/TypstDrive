<script lang="ts">
    import { onMount } from 'svelte';
    import Icon from '@iconify/svelte';
    let account=$state<{configured:boolean;connected:boolean;login?:string;installation_url?:string}|null>(null);
    let error=$state('');let busy=$state(false);
    async function load(){try{const r=await fetch('/api/github/account');if(!r.ok)throw new Error(await r.text());account=await r.json();}catch(e){error=e instanceof Error?e.message:'Unable to load GitHub account';}}
    onMount(()=>{load();});
    async function disconnect(){busy=true;error='';try{const r=await fetch('/api/github/account',{method:'DELETE'});if(!r.ok)throw new Error(await r.text());await load();}catch(e){error=e instanceof Error?e.message:'Unable to disconnect';}busy=false;}
</script>
<section class="rounded-2xl border border-gray-200 dark:border-white/10 bg-white dark:bg-zinc-900 p-6 shadow-sm">
    <h2 class="flex items-center gap-3 text-xl font-semibold"><Icon icon="mdi:github" class="text-2xl" />GitHub</h2>
    <p class="mt-2 text-sm text-gray-500">Connect your own account to pull and push documents and projects. Your GitHub repository permissions still apply.</p>
    {#if error}<p role="alert" class="mt-3 text-sm text-red-500">{error}</p>{/if}
    {#if account}
        {#if !account.configured}<p class="mt-4 text-sm text-gray-500">GitHub integration is awaiting server setup.</p>
        {:else if account.connected}<p class="mt-4 text-sm">Connected as <strong>{account.login}</strong></p>{#if account.installation_url}<a href={account.installation_url} target="_blank" rel="noopener noreferrer" class="mt-4 mr-3 inline-flex items-center gap-2 rounded-lg bg-gray-900 dark:bg-white px-4 py-2 text-sm font-medium text-white dark:text-gray-900"><Icon icon="mdi:github" />Choose repositories</a>{/if}<button disabled={busy} onclick={disconnect} class="mt-4 rounded-lg border border-gray-300 dark:border-white/20 px-4 py-2 text-sm">Disconnect GitHub</button>
        {:else}<a href="/api/github/connect" class="mt-4 inline-flex items-center gap-2 rounded-lg bg-gray-900 dark:bg-white px-4 py-2 text-sm font-medium text-white dark:text-gray-900"><Icon icon="mdi:github" />Connect GitHub</a>{/if}
    {/if}
</section>
