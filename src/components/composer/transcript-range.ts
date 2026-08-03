// 语音 transcript 可编辑区间：外部手工编辑先重定位区间，ASR 更新再三方合并进该区间。
interface EditBounds {
  oldStart: number;
  oldEnd: number;
  newEnd: number;
}

function editBounds(before: string, after: string): EditBounds {
  let prefix = 0;
  while (prefix < before.length && prefix < after.length && before[prefix] === after[prefix])
    prefix++;
  let suffix = 0;
  while (
    suffix < before.length - prefix &&
    suffix < after.length - prefix &&
    before[before.length - 1 - suffix] === after[after.length - 1 - suffix]
  )
    suffix++;
  return { oldStart: prefix, oldEnd: before.length - suffix, newEnd: after.length - suffix };
}

function mergeLocalEdit(base: string, local: string, remote: string): string {
  if (base === local) return remote;
  const edit = editBounds(base, local);
  const inserted = local.slice(edit.oldStart, edit.newEnd);
  const suffix = base.slice(edit.oldEnd);
  const remoteStart = Math.min(edit.oldStart, remote.length);
  let remoteEnd = Math.min(remote.length, remoteStart + edit.oldEnd - edit.oldStart);
  if (suffix) {
    const found = remote.indexOf(suffix, remoteStart);
    if (found >= 0) remoteEnd = found;
  }
  return remote.slice(0, remoteStart) + inserted + remote.slice(remoteEnd);
}

export function createTranscriptRange(opts: {
  getText: () => string;
  setText: (text: string) => void;
  afterChange: () => void;
}) {
  let rendered = "";
  let start = 0;
  let end = 0;
  let raw = "";
  let displayed = "";

  function reset() {
    rendered = opts.getText();
    start = rendered.length;
    end = start;
    raw = "";
    displayed = "";
  }

  function reconcile(): string {
    const current = opts.getText();
    if (current === rendered) return current;
    const edit = editBounds(rendered, current);
    const delta = edit.newEnd - edit.oldEnd;
    if (edit.oldStart === edit.oldEnd) {
      if (edit.oldStart < start || (start === end && edit.oldStart === start)) {
        start += delta;
        end += delta;
      } else if (edit.oldStart < end) end += delta;
    } else if (edit.oldEnd <= start) {
      start += delta;
      end += delta;
    } else if (edit.oldStart < end) {
      if (edit.oldStart < start) start = edit.oldStart;
      end = edit.oldEnd < end ? end + delta : edit.newEnd;
    }
    displayed = current.slice(start, end);
    rendered = current;
    return current;
  }

  function render(nextRaw: string) {
    const current = reconcile();
    const nextDisplayed = mergeLocalEdit(raw, displayed, nextRaw);
    rendered = current.slice(0, start) + nextDisplayed + current.slice(end);
    end = start + nextDisplayed.length;
    raw = nextRaw;
    displayed = nextDisplayed;
    opts.setText(rendered);
    opts.afterChange();
  }

  return { reset, render };
}
