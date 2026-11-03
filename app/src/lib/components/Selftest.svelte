<script lang="ts">
  // 開發建置才有：確認這個平台的金鑰庫真的能 put／get／delete（Android 驗 Keystore 與 application context 的接線）。
  // `pnpm dev`／`tauri dev` 自動開啟；打包 debug APK 時設 VITE_PR_RUNNERS_SELFTEST=1。
  import { invoke } from "@tauri-apps/api/core";

  const enabled = import.meta.env.DEV || import.meta.env.VITE_PR_RUNNERS_SELFTEST === "1";
  let result = $state("");

  async function run() {
    try {
      await invoke("secret_selftest");
      result = "金鑰庫自我檢測：OK";
    } catch (e) {
      result = `金鑰庫自我檢測失敗：${e}`;
    }
  }
</script>

{#if enabled}
  <div class="selftest">
    <button class="link" onclick={run}>金鑰庫自我檢測</button>
    {#if result}<p role="status" data-testid="selftest-result">{result}</p>{/if}
  </div>
{/if}
