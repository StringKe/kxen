// 会话级模型：切换写 session metadata；全局默认走设置页 config.set_role，保持原路径。
import { createEffect, createSignal } from "solid-js";
import { client } from "./client";
import { currentModel } from "./chat";
import { modelsCatalog } from "./models";
import { createSeqGuard } from "./async-guard";

// 草稿态（会话未落库）的模型选择无处可写：暂存于此，会话创建后写入其 metadata；
// "default" = 暂存的是「跟随全局默认」（清除覆盖），与具体模型二选一
type SessionModelPick = { provider: string; model: string } | "default";

let draftPick: SessionModelPick | null = null;
const pendingSessionPick = new Map<string, SessionModelPick>();

export async function sessionSetModel(
  sessionId: string,
  provider: string,
  model: string,
): Promise<void> {
  if (!sessionId) {
    draftPick = { provider, model };
    return;
  }
  await client.rpc("session.set_model", { id: sessionId, provider, model });
  pendingSessionPick.delete(sessionId);
}

/** 清除会话级覆盖，跟随全局默认（后端约定：provider/model 同缺 = 清除）。 */
export async function sessionFollowGlobalModel(sessionId: string): Promise<void> {
  if (!sessionId) {
    draftPick = "default";
    return;
  }
  await client.rpc("session.set_model", { id: sessionId });
  pendingSessionPick.delete(sessionId);
}

/** 放弃当前草稿时清除未落库选择，不能泄漏到下一份草稿。 */
export function resetDraftModel(): void {
  draftPick = null;
}

/** 会话落库后回写草稿选择；失败转存为该 session 的待重试选择并向上抛，发送链必须中止。 */
export async function applyDraftModel(sessionId: string, includeDraft = true): Promise<void> {
  const fromDraft = includeDraft ? draftPick : null;
  const pick = pendingSessionPick.get(sessionId) ?? fromDraft;
  if (!pick) return;
  try {
    if (pick === "default") await sessionFollowGlobalModel(sessionId);
    else await sessionSetModel(sessionId, pick.provider, pick.model);
  } catch (error) {
    pendingSessionPick.set(sessionId, pick);
    if (draftPick === pick) draftPick = null; // 所有权已迁到已创建 session，避免再污染下一草稿
    throw error;
  }
  pendingSessionPick.delete(sessionId);
  if (draftPick === pick) draftPick = null;
}

/** 当前 session 生效模型的 ctx 窗（composer token 估算分级用）；后端不可达/目录未命中返回 0，调用方自定回退。 */
export function createSessionCtxWindow(getSid: () => string): () => number {
  const [ctx, setCtx] = createSignal(0);
  const guard = createSeqGuard();
  createEffect(() => {
    const sid = getSid();
    const request = guard.next();
    void currentModel(sid || undefined)
      .then(async (m) => {
        // 不引 modelOf：目录结构在此处一次收窄，避免再引入一套模型查找依赖。
        const hit = (await modelsCatalog().catch(() => []))
          .find((p) => p.provider === m.provider)
          ?.models.find((x) => x.id === m.model);
        if (guard.isCurrent(request) && getSid() === sid) setCtx(hit?.context ?? 0);
      })
      .catch(() => {
        if (guard.isCurrent(request) && getSid() === sid) setCtx(0);
      });
  });
  return ctx;
}
