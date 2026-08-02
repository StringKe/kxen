// 空态：logo + 「打开项目目录」引导卡 + 四快捷卡。
// 入场动画只在 app 首次挂载播放；之后点新会话直接静态到位
//（清空与空态同帧完成，避免空白 + 闪入的割裂感）。
import { onMount } from "solid-js";
import { CalendarClock, FolderOpen, Target, Users, Workflow } from "lucide-solid";
import { insertComposerText } from "../lib/composer-bus";
import { openProjectDir } from "../lib/open-project";

const CARDS = [
  {
    icon: Target,
    title: "write-goal",
    desc: "定义带完成判据的目标，自动推进直到验证通过",
    prompt: "/write-goal ",
  },
  {
    icon: CalendarClock,
    title: "schedule",
    desc: "为当前会话创建一次性或 cron 定时任务",
    prompt: "请为当前会话创建一个定时任务：",
  },
  {
    icon: Workflow,
    title: "workflow",
    desc: "编排独立子任务并行执行，汇总后统一验证",
    prompt: "/ultracode ",
  },
  {
    icon: Users,
    title: "agent teams",
    desc: "创建多模型 teammates，各自使用独立上下文协作",
    prompt: "请为这个任务创建一个 agent team：",
  },
];

let heroPlayed = false;

export default function EmptyHero() {
  const animated = !heroPlayed;
  onMount(() => {
    heroPlayed = true;
  });
  return (
    <div class="pt-16 space-y-8 w-full">
      <div class={animated ? "empty-hero" : ""} classList={{ "flex items-center gap-4": true }}>
        <img
          src="/icon.png"
          alt="kxen"
          class="w-14 h-14 rounded-2xl shadow-lg shadow-indigo-500/20"
        />
        <div>
          <div class="text-lg font-semibold tracking-tight">kxen</div>
          <div class="text-xs text-[var(--text-dim)]">多模型并行工作 · 目标驱动 · 团队编排</div>
        </div>
      </div>
      <div class="grid grid-cols-2 gap-2.5">
        {/* 新用户首屏引导：四张 prompt 卡之前先给「打开项目」入口，
            否则首发消息可能直接跑在回退的家目录 workspace */}
        <button
          type="button"
          class={`rounded-xl border border-[var(--border)] bg-[var(--bg-raised)] p-3.5 space-y-1.5 ${animated ? "empty-card" : ""}`}
          style={animated ? "animation-delay: 80ms" : ""}
          title="选择本地项目文件夹"
          onClick={() => void openProjectDir()}
        >
          <FolderOpen size={16} class="text-[var(--accent-hover)]" />
          <div class="text-left text-xs font-medium">打开项目目录</div>
          <div class="text-left text-xs leading-snug text-[var(--text-faint)]">
            选择本地项目文件夹，会话在该目录下运行
          </div>
        </button>
        {CARDS.map((c, i) => (
          <button
            type="button"
            class={`rounded-xl border border-[var(--border)] bg-[var(--bg-raised)] p-3.5 space-y-1.5 ${animated ? "empty-card" : ""}`}
            style={animated ? `animation-delay: ${80 + (i + 1) * 50}ms` : ""}
            title={`填入 ${c.title}`}
            onClick={() => insertComposerText(c.prompt)}
          >
            <c.icon size={16} class="text-[var(--accent-hover)]" />
            <div class="text-left text-xs font-medium font-mono">{c.title}</div>
            <div class="text-left text-xs leading-snug text-[var(--text-faint)]">{c.desc}</div>
          </button>
        ))}
      </div>
      <div
        class={`text-xs text-[var(--text-faint)] ${animated ? "empty-card" : ""}`}
        style={animated ? "animation-delay: 300ms" : ""}
      >
        输入消息开始 · @ 引用 · / 命令 · # 沉淀 · 粘贴图片
      </div>
    </div>
  );
}
