<script lang="ts">
  import type { Snapshot } from "$lib/generated/Snapshot";
  import { UPDATING_NOTE } from "$lib/loading";
  import { SKEW_NOTE, accountNote, formatClock, hasSkew } from "$lib/view";

  let { snapshot, onrelogin }: { snapshot: Snapshot; onrelogin: () => void } = $props();
  const lastUpdated = $derived(
    Math.max(0, ...snapshot.accounts.flatMap((a) => a.scopes.map((s) => s.last_updated_at))),
  );
</script>

<footer class="status">
  {#each snapshot.accounts as a (a.account_id)}
    {@const note = accountNote(a.status)}
    {#if note}
      <p class="acct-note">
        <b>{a.login}</b> ·
        {#if a.status === "NeedsReauth"}
          <button class="link" onclick={onrelogin}>{note}</button>
        {:else}
          {note}
        {/if}
      </p>
    {/if}
  {/each}
  {#if snapshot.updating}
    <p class="updated" role="status">{UPDATING_NOTE}</p>
  {:else if lastUpdated > 0}
    <p class="updated">最後更新 {formatClock(lastUpdated)}</p>
  {/if}
  {#if hasSkew(snapshot)}<p class="skew">{SKEW_NOTE}</p>{/if}
</footer>
