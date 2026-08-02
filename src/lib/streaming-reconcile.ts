// streaming 态收放的真源对账。
// streamingSid 只由发送动作乐观臂上（即时反馈）；之后的保持/重臂/收回全部由
// 运行真源（session.list 的 running 快照）决定，不由单帧决定：
// - 后端先发布终态再 spawn 续跑（ws/run_finalize.rs），收到 done 即清会让续跑 run
//   全程无进度/停止钮——done 只当真源核对扳机，running=true 就保持/重臂；
// - 草稿态首发 ACL 订阅竞态下快速终态的 done 帧被丢弃、onDone 永不触发——
//   session.update（RunGuard 存亡广播）兜底一轮真源核对收回。
import { client } from "./client";
import { sessionRunning } from "./chat";

/** RPC 失败（running=null=未知）时的兜底：done 路径持终态帧（帧在 = 本 run 已终）按终态收回；
 *  事件/resync 路径保守保留，等下轮事件/resync 再核。 */
export type UnknownPolicy = "keep" | "clear";

export function createStreamingReconcile(deps: {
  activeSessionId: () => string;
  streamingSid: () => string;
  setStreamingSid: (sid: string) => void;
}) {
  const reconcile = (sid: string, onUnknown: UnknownPolicy) => {
    void sessionRunning(sid).then((running) => {
      if (deps.activeSessionId() !== sid) return;
      if (running === true) {
        if (deps.streamingSid() !== sid) deps.setStreamingSid(sid);
      } else if (running === false || onUnknown === "clear") {
        if (deps.streamingSid() === sid) deps.setStreamingSid("");
      }
    });
  };

  /** session.update（RunGuard 存亡广播）驱动真源核对：续跑重臂、快速终态丢帧收回。返回注销。 */
  const mountSource = () =>
    client.stream("session.update").on(() => {
      const sid = deps.activeSessionId();
      if (sid) reconcile(sid, "keep");
    });

  return { reconcile, mountSource };
}
