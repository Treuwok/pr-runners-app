import { describe, expect, it } from "vitest";
import { finishSetup, startDeviceLogin, waitForInstall, type OnboardingDeps } from "./onboarding";
import type { DeviceCode } from "./generated/DeviceCode";
import type { ScopeChoice } from "./generated/ScopeChoice";

const code: DeviceCode = {
  device_code: "dev-1",
  user_code: "ABCD-1234",
  verification_uri: "https://github.com/login/device",
  interval: 5,
  expires_at: 9_999_999_999,
};

function choice(name: string, selected = false): ScopeChoice {
  return { name, kind: "Org", selected };
}

/** 依指令名稱回應的假 invoke；記下每一次呼叫，方便檢查送了什麼。 */
function fakeDeps(handlers: Record<string, (args?: Record<string, unknown>) => unknown>) {
  const calls: { cmd: string; args?: Record<string, unknown> }[] = [];
  const opened: string[] = [];
  const deps: OnboardingDeps = {
    invoke: async <T>(cmd: string, args?: Record<string, unknown>) => {
      calls.push({ cmd, args });
      const h = handlers[cmd];
      if (!h) throw new Error(`沒有預期會呼叫 ${cmd}`);
      return (await h(args)) as T;
    },
    openUrl: async (url) => {
      opened.push(url);
      return null;
    },
    sleep: async () => {},
  };
  return { deps, calls, opened };
}

describe("startDeviceLogin", () => {
  it("取得代碼後複製到剪貼簿並打開 GitHub", async () => {
    const { deps, calls, opened } = fakeDeps({ login_start: () => code, copy_login_code: () => undefined });
    const r = await startDeviceLogin(deps);
    expect(r).toEqual({ code, copied: true, openError: null });
    expect(calls.map((c) => c.cmd)).toEqual(["login_start", "copy_login_code"]);
    expect(opened).toEqual([code.verification_uri]);
  });

  it("複製失敗仍然打開 GitHub，讓畫面改請使用者自己輸入", async () => {
    const { deps, opened } = fakeDeps({
      login_start: () => code,
      copy_login_code: () => {
        throw new Error("no clipboard");
      },
    });
    const r = await startDeviceLogin(deps);
    expect(r.copied).toBe(false);
    expect(opened).toEqual([code.verification_uri]);
  });

  it("瀏覽器打不開時回傳原因，代碼照樣顯示", async () => {
    const { deps } = fakeDeps({ login_start: () => code, copy_login_code: () => undefined });
    deps.openUrl = async () => "blocked";
    const r = await startDeviceLogin(deps);
    expect(r).toEqual({ code, copied: true, openError: "blocked" });
  });
});

describe("finishSetup", () => {
  it("已經裝到帳號或 org：全部勾選並直接完成，不再問範圍", async () => {
    const { deps, calls } = fakeDeps({
      list_scopes: () => [choice("acme"), choice("jdoe")],
      set_scopes: () => undefined,
    });
    expect(await finishSetup(deps, 7)).toBe("done");
    expect(calls.at(-1)).toEqual({ cmd: "set_scopes", args: { accountId: 7, selected: ["acme", "jdoe"] } });
  });

  it("重新登入時保留原本的勾選", async () => {
    const { deps, calls } = fakeDeps({
      list_scopes: () => [choice("acme", true), choice("jdoe", false)],
      set_scopes: () => undefined,
    });
    expect(await finishSetup(deps, 7)).toBe("done");
    expect(calls.at(-1)?.args).toEqual({ accountId: 7, selected: ["acme"] });
  });

  it("還沒裝到任何地方：回報需要安裝，不寫入範圍", async () => {
    const { deps, calls } = fakeDeps({ list_scopes: () => [] });
    expect(await finishSetup(deps, 7)).toBe("install");
    expect(calls.map((c) => c.cmd)).toEqual(["list_scopes"]);
  });
});

describe("waitForInstall", () => {
  it("每隔一段時間檢查一次，裝好就完成", async () => {
    let checks = 0;
    const { deps } = fakeDeps({
      list_scopes: () => (++checks < 3 ? [] : [choice("acme")]),
      set_scopes: () => undefined,
    });
    expect(await waitForInstall(deps, 7, () => false)).toBe("done");
    expect(checks).toBe(3);
  });

  it("暫時查不到（網路錯誤）就下一輪再試", async () => {
    let checks = 0;
    const { deps } = fakeDeps({
      list_scopes: () => {
        if (++checks === 1) throw new Error("offline");
        return [choice("acme")];
      },
      set_scopes: () => undefined,
    });
    expect(await waitForInstall(deps, 7, () => false)).toBe("done");
  });

  it("離開畫面就停止，不再呼叫 GitHub", async () => {
    let cancelled = false;
    let checks = 0;
    const { deps } = fakeDeps({
      list_scopes: () => {
        checks++;
        cancelled = true;
        return [];
      },
    });
    expect(await waitForInstall(deps, 7, () => cancelled)).toBe("cancelled");
    expect(checks).toBe(1);
  });
});
