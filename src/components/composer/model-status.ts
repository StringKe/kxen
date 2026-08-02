import { createEffect, createSignal, onMount } from "solid-js";
import { currentModel } from "../../lib/chat";
import { createSeqGuard } from "../../lib/async-guard";
import { activeSessionId } from "../../lib/state";
import { errText } from "../err-text";

export function createModelStatus() {
  const [cur, setCur] = createSignal({ provider: "", model: "" });
  const [curErr, setCurErr] = createSignal("");
  const [globalDef, setGlobalDef] = createSignal({ provider: "", model: "" });
  const [globalErr, setGlobalErr] = createSignal("");
  const currentGuard = createSeqGuard();
  const globalGuard = createSeqGuard();

  const reloadCurrent = async (sid = activeSessionId(), preserve = false): Promise<string> => {
    const request = currentGuard.next();
    if (!preserve) setCur({ provider: "", model: "" });
    setCurErr("");
    try {
      const model = await currentModel(sid || undefined);
      if (currentGuard.isCurrent(request) && activeSessionId() === sid) {
        setCur({ provider: model.provider, model: model.model });
        setCurErr("");
      }
      return "";
    } catch (error) {
      const message = errText(error);
      if (currentGuard.isCurrent(request) && activeSessionId() === sid) setCurErr(message);
      return message;
    }
  };

  const reloadGlobal = async () => {
    const request = globalGuard.next();
    try {
      const model = await currentModel();
      if (!globalGuard.isCurrent(request)) return;
      setGlobalDef({ provider: model.provider, model: model.model });
      setGlobalErr("");
    } catch (error) {
      if (globalGuard.isCurrent(request)) setGlobalErr(errText(error));
    }
  };

  createEffect(() => void reloadCurrent(activeSessionId()));
  onMount(() => void reloadGlobal());
  return { cur, setCur, curErr, setCurErr, globalDef, globalErr, reloadCurrent, reloadGlobal };
}
