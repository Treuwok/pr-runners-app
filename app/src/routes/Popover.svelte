<script lang="ts">
  // 選單列小視窗（400 寬、最高 640）：和主視窗共用同一份內容，底部是三個項目。
  import { invoke } from "@tauri-apps/api/core";
  import { onMount } from "svelte";
  import { applyPlatformClass, startStore, store } from "$lib/store.svelte";
  import { formatClock } from "$lib/view";
  import RunnersView from "./RunnersView.svelte";

  // 量內層內容（不受視窗高度限制）：外層 .popover 會被視窗高度截住，只有 scrollHeight 變大時 ResizeObserver 不會觸發
  let inner: HTMLDivElement | undefined = $state();
  const PADDING = 16; // .popover 上下 padding（10 + 6）

  onMount(() => {
    let stop = () => {};
    applyPlatformClass();
    startStore().then((f) => (stop = f));
    // 高度依內容：量到多高就請 Rust 端調成多高（它會限制在 640 以內）
    const ro = new ResizeObserver(() => {
      if (inner) void invoke("resize_popover", { height: inner.getBoundingClientRect().height + PADDING }).catch(() => {});
    });
    if (inner) ro.observe(inner);
    return () => {
      stop();
      ro.disconnect();
    };
  });

  const snap = $derived(store.snapshot);
</script>

<div class="popover">
<div bind:this={inner}>
  {#if !store.loaded}
    <div class="skeleton" aria-busy="true" aria-label="載入中">
      {#each [0, 1, 2] as i (i)}<div class="sk-row"></div>{/each}
    </div>
  {:else if !snap || snap.accounts.length === 0}
    <p class="empty">還沒有登入。</p>
  {:else}
    <RunnersView
      snapshot={snap}
      limit={5}
      onmore={() => invoke("show_main", { settings: false })}
      onopen={(runnerKey) => invoke("show_runner_history", { runnerKey })}
      onrelogin={() => invoke("show_main", { settings: false })}
    />
  {/if}
  <div class="menu">
    <div class="sep"></div>
    <button class="mi" onclick={() => invoke("show_main", { settings: false })}>打開主視窗…</button>
    <button class="mi" onclick={() => invoke("show_main", { settings: true })}>設定…</button>
    <button class="mi" onclick={() => invoke("quit_app")}>結束</button>
  </div>
</div>
</div>

<style>
  .popover { padding: 10px 0 6px; max-height: 100vh; overflow-y: auto; }
  .menu { position: sticky; bottom: 0; background: var(--bg); padding-bottom: 6px; }
  .sep { height: 1px; background: var(--hair); margin: 6px 16px; }
  .mi { display: block; width: 100%; padding: 6px 16px; }
  .mi:hover { background: var(--chip); }
</style>
