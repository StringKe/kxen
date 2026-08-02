import { sessionUpdateMeta, type SessionMeta } from "./chat";
import { formatError } from "./error-text";

export interface SessionReorderResult {
  saveError?: unknown;
  rollbackFailures: string[];
}

/** 单条更新无事务能力：任何失败都把整组原序号补偿写回。 */
export async function reorderSessionGroup(
  sessions: SessionMeta[],
  sourceId: string,
  targetId: string,
): Promise<SessionReorderResult | null> {
  const list = sessions.filter((session) => !session.pinned);
  const from = list.findIndex((session) => session.id === sourceId);
  const to = list.findIndex((session) => session.id === targetId);
  if (from < 0 || to < 0 || from === to) return null;
  const original = new Map(list.map((session) => [session.id, session.sort_order ?? null]));
  const moved = list.splice(from, 1)[0]!;
  list.splice(to, 0, moved);

  let saveError: unknown;
  for (let index = 0; index < list.length; index++) {
    try {
      await sessionUpdateMeta(list[index]!.id, { sort_order: index + 1 });
    } catch (error) {
      saveError = error;
      break;
    }
  }
  if (!saveError) return { rollbackFailures: [] };

  const rollbackFailures: string[] = [];
  for (const session of list) {
    try {
      await sessionUpdateMeta(session.id, { sort_order: original.get(session.id) ?? null });
    } catch (error) {
      rollbackFailures.push(`${session.id}: ${formatError(error)}`);
    }
  }
  return { saveError, rollbackFailures };
}
