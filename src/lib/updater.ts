import { createSignal } from "solid-js";
import { getVersion } from "@tauri-apps/api/app";
import { relaunch } from "@tauri-apps/plugin-process";
import { check } from "@tauri-apps/plugin-updater";
import { flashOk } from "./flash";

export type AvailableUpdate = NonNullable<Awaited<ReturnType<typeof check>>>;

// 启动静默检查与设置页手动检查共用同一状态源：只查一次，UpdateSection 直接读这里
const [availableUpdate, setAvailableUpdate] = createSignal<AvailableUpdate | null>(null);
export { availableUpdate };

let checked = false;
let flight: Promise<AvailableUpdate | null> | null = null;

export async function currentVersion(): Promise<string> {
  return getVersion();
}

/** 检查更新并写入共享状态；并发调用共享同一 flight，不重复请求。失败不置 checked，下次可重试。 */
export function checkForUpdate(): Promise<AvailableUpdate | null> {
  flight ??= check()
    .then((update) => {
      checked = true;
      setAvailableUpdate(update);
      return update;
    })
    .finally(() => {
      flight = null;
    });
  return flight;
}

/** 启动时静默检查一次：失败吞掉（不打错误弹窗）；有更新只 toast 提示并填充共享状态，不打断用户。 */
export function autoCheckOnStartup(): void {
  if (checked || flight) return;
  void checkForUpdate()
    .then((update) => {
      if (update) flashOk(`发现新版本 ${update.version}，可在 设置 > 应用更新 安装`);
    })
    .catch(() => {});
}

export async function installUpdate(update: AvailableUpdate): Promise<void> {
  await update.downloadAndInstall();
  await relaunch();
}
