<script lang="ts">
  import type { Snapshot } from "$lib/generated/Snapshot";
  import { openUrl } from "$lib/store.svelte";
  import { repoName } from "$lib/view";

  let { snapshot }: { snapshot: Snapshot } = $props();
  // key 帶帳號：兩個帳號看同一個 org 時 job key 會重複
  const jobs = $derived(
    snapshot.accounts.flatMap((a) => a.scopes.flatMap((s) => s.jobs.map((j) => ({ id: `${a.account_id}/${s.name}/${j.key}`, j })))),
  );
  const stateText = { Queued: "排隊中", Waiting: "等待核准", InProgress: "進行中" } as const;
</script>

{#each jobs as { id, j } (id)}
  <button class="row job" onclick={() => openUrl(j.url)}>
    <span class="dot {j.state === 'InProgress' ? 'busy' : 'idle'}" aria-hidden="true"></span>
    <span class="main">
      <span class="n">{repoName(j.repo)} · {j.workflow} → {j.job}</span>
      <span class="j">
        {stateText[j.state]} · {j.pr_tag}
        {#if j.hosted}· GitHub-hosted{:else if j.runner_name}· {j.runner_name}{/if}
      </span>
    </span>
  </button>
{:else}
  <p class="empty">目前沒有進行中或排隊中的 job</p>
{/each}
