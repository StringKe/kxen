//! 语音输入多引擎：apple（Speech.framework 本地识别，主引擎）+ provider（OpenAI 兼容转写，降级链）。

pub mod apple;
pub mod objc;
pub mod provider;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineStatus {
    pub id: String,
    pub label: String,
    /// ready | needs_auth | unconfigured | unavailable
    pub status: String,
    pub detail: String,
}

/// 引擎状态总览（设置页语音区 + mic 菜单数据源）。
pub fn engines(config: &crate::core::config::Config, store: &crate::auth::credential::AuthStore) -> Vec<EngineStatus> {
    let mut out = vec![apple::status()];
    out.extend(provider::statuses(config, store));
    out
}

// ---------------- 活跃 PTT 会话（按 chat session 键控，多会话并发 PTT 互不打断） ----------------

// ObjC 对象句柄跨线程存放（Speech/AVAudio 回调均走框架队列，stop 路径单线程）
struct SendWrap<T>(T);
unsafe impl<T> Send for SendWrap<T> {}
unsafe impl<T> Sync for SendWrap<T> {}

enum Active {
    /// token 是泵线程身份：槽位被替换/移除后旧泵 ptr_eq 不过立即退出
    /// （旧实现单槽无守卫：旧泵永不退出，会吸新会话事件造成串流）
    Apple {
        session: SendWrap<apple::MicSession>,
        token: std::sync::Arc<()>,
    },
    Record {
        session: SendWrap<provider::RecordSession>,
        provider: String,
    },
    #[cfg(test)]
    Dummy(u32),
}

impl Active {
    fn cancel(self) {
        match self {
            Self::Apple { session, .. } => session.0.cancel(),
            Self::Record { session, .. } => session.0.cancel(),
            #[cfg(test)]
            Self::Dummy(_) => {}
        }
    }
}

static ACTIVE: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<String, Active>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

/// PTT 按下：按 [主引擎, ...fallback] 顺序尝试启动（apple 流式 / provider 录音），partial 经 bus 泵给前端。
/// 同 session 重复 start = 替换；不同 session 互不打断。
pub fn start(
    config: &crate::core::config::VoiceConfig,
    store: &crate::auth::credential::AuthStore,
    locale: &str,
    bus: crate::core::event::EventBus,
    session_id: &str,
) -> Result<String, String> {
    let mut errors: Vec<String> = Vec::new();
    for engine in std::iter::once(config.engine.as_str()).chain(config.fallback.iter().map(String::as_str)) {
        match start_one(engine, store, locale, &bus, session_id) {
            Ok(started) => return Ok(started),
            Err(e) => errors.push(format!("{engine}: {e}")),
        }
    }
    Err(format!("全部语音引擎不可用（{}）", errors.join("; ")))
}

fn start_one(
    engine: &str,
    store: &crate::auth::credential::AuthStore,
    locale: &str,
    bus: &crate::core::event::EventBus,
    session_id: &str,
) -> Result<String, String> {
    // 同 session 重复 start = 替换：旧槽先移出（旧泵 ptr_eq 不过随即退出）
    let previous = { crate::core::shared::lock(&ACTIVE).remove(session_id) };
    if let Some(previous) = previous {
        previous.cancel();
    }
    match engine {
        "apple" => {
            let session = apple::start_mic(locale)?;
            let token = std::sync::Arc::new(());
            let token_pump = token.clone();
            crate::core::shared::lock(&ACTIVE).insert(session_id.to_string(), Active::Apple { session: SendWrap(session), token });
            let bus = bus.clone();
            let key = session_id.to_string();
            std::thread::spawn(move || {
                loop {
                    let events = {
                        let map = crate::core::shared::lock(&ACTIVE);
                        match map.get(&key) {
                            Some(Active::Apple { session, token }) if std::sync::Arc::ptr_eq(token, &token_pump) => session.0.drain(),
                            _ => break,
                        }
                    };
                    for e in events {
                        bus.publish(crate::core::event::Event::LlmDelta(event_payload(e, &key)));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(80));
                }
            });
            Ok("apple".into())
        }
        #[cfg(test)]
        "dummy" => {
            // 测试引擎：避开麦克风硬件验证槽位语义，序号用于区分替换前后的槽
            static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            crate::core::shared::lock(&ACTIVE).insert(session_id.to_string(), Active::Dummy(n));
            Ok("dummy".into())
        }
        other => {
            if !provider::configured(store, other) {
                return Err(format!("{other} 未配置 API key"));
            }
            let session = provider::start_recording()?;
            crate::core::shared::lock(&ACTIVE)
                .insert(session_id.to_string(), Active::Record { session: SendWrap(session), provider: other.to_string() });
            Ok(other.to_string())
        }
    }
}

/// 转写事件统一携带 session_id（ws/stream.rs 的 session ACL 按它准入）；
/// 空 id 是旧全局通道：不带键，否则会被 ACL 当成未知 session 拦掉。
fn event_payload(e: apple::SessionEvent, session_id: &str) -> serde_json::Value {
    let mut payload = match e {
        apple::SessionEvent::Partial(t) => serde_json::json!({"kind": "voice.partial", "text": t}),
        apple::SessionEvent::Final(t) => serde_json::json!({"kind": "voice.final", "text": t}),
        apple::SessionEvent::Error(m) => serde_json::json!({"kind": "voice.error", "message": m}),
    };
    if !session_id.is_empty() {
        payload.as_object_mut().expect("voice payload").insert("session_id".into(), serde_json::Value::String(session_id.into()));
    }
    payload
}

/// PTT 松开：只停自己槽（别的 session 继续录）。apple 先出本地终稿，有就绪云引擎则云转写升级（Wispr 双轨）；失败回落本地。
pub async fn stop(
    config: &crate::core::config::Config,
    store: &crate::auth::credential::AuthStore,
    session_id: &str,
) -> Result<Option<String>, String> {
    // 先取槽再 match：guard 临时量若写在 scrutinee 里会活过 arm 内的 await（非 Send）
    let slot = crate::core::shared::lock(&ACTIVE).remove(session_id);
    match slot {
        None => Ok(None),
        Some(Active::Apple { session, .. }) => {
            let (local, wav) = session.0.stop();
            // 云转写终稿：fallback 链里第一个有 key 的 provider（含 audio 自定义）
            if let Some(path) = wav {
                let cloud = match first_ready_cloud(config, store) {
                    Some(engine) => {
                        let r = provider::transcribe_file(config, store, &engine, &path).await.ok();
                        let _ = std::fs::remove_file(&path);
                        r
                    }
                    None => {
                        let _ = std::fs::remove_file(&path);
                        None
                    }
                };
                return Ok(cloud.or(local));
            }
            Ok(local)
        }
        Some(Active::Record { session, provider }) => {
            let (path, _dur) = session.0.stop()?;
            let text = provider::transcribe_file(config, store, &provider, &path).await;
            let _ = std::fs::remove_file(&path);
            text.map(Some)
        }
        #[cfg(test)]
        Some(Active::Dummy(_)) => Ok(None),
    }
}

/// Session 生命周期终点：停止并移除仍占用麦克风的 PTT 槽。
pub fn drop_session(session_id: &str) {
    let active = { crate::core::shared::lock(&ACTIVE).remove(session_id) };
    if let Some(active) = active {
        active.cancel();
    }
}

/// 云终稿引擎选择：fallback 链优先，其次 openai/xai/自定义 audio 里第一个有 key 的。
fn first_ready_cloud(config: &crate::core::config::Config, store: &crate::auth::credential::AuthStore) -> Option<String> {
    let mut candidates: Vec<String> = config.voice.fallback.clone();
    candidates.extend(["openai", "xai"].map(String::from));
    candidates.extend(
        config.custom_providers.iter().filter(|(_, d)| d.capabilities.iter().any(|c| c == "audio")).map(|(n, _)| format!("custom:{n}")),
    );
    candidates.into_iter().find(|id| provider::configured(store, id))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_config() -> crate::core::config::VoiceConfig {
        crate::core::config::VoiceConfig { engine: "dummy".into(), ..Default::default() }
    }

    fn dummy_slot(session_id: &str) -> Option<u32> {
        match crate::core::shared::lock(&ACTIVE).get(session_id) {
            Some(Active::Dummy(n)) => Some(*n),
            _ => None,
        }
    }

    #[tokio::test]
    async fn session_slots_are_independent() {
        let store = crate::auth::credential::AuthStore::new();
        let bus = crate::core::event::EventBus::default();
        start(&dummy_config(), &store, "zh-CN", bus.clone(), "slot-a").expect("start a");
        start(&dummy_config(), &store, "zh-CN", bus.clone(), "slot-b").expect("start b");
        // 同 session 重复 start = 替换（序号变），别的槽不动
        let before = dummy_slot("slot-a");
        start(&dummy_config(), &store, "zh-CN", bus.clone(), "slot-a").expect("restart a");
        assert_ne!(before, dummy_slot("slot-a"), "同 session 重复 start 应替换旧槽");
        assert!(dummy_slot("slot-b").is_some(), "别的 session 槽位不得受影响");
        // stop 只 remove 自己槽
        let text = stop(&crate::core::config::Config::default(), &store, "slot-a").await.expect("stop a");
        assert_eq!(text, None);
        assert!(dummy_slot("slot-a").is_none());
        assert!(dummy_slot("slot-b").is_some(), "stop 别的 session 不得受影响");
        // 未知 session 无操作
        let text = stop(&crate::core::config::Config::default(), &store, "slot-unknown").await.expect("stop unknown");
        assert_eq!(text, None);
        crate::core::shared::lock(&ACTIVE).clear();
    }

    #[test]
    fn drop_session_reclaims_active_slot() {
        let store = crate::auth::credential::AuthStore::new();
        start(&dummy_config(), &store, "zh-CN", crate::core::event::EventBus::default(), "voice-delete").unwrap();
        assert!(dummy_slot("voice-delete").is_some());
        drop_session("voice-delete");
        assert!(dummy_slot("voice-delete").is_none());
    }

    #[test]
    fn event_payload_carries_session_id() {
        let p = event_payload(apple::SessionEvent::Partial("你好".into()), "s1");
        assert_eq!(p["kind"], "voice.partial");
        assert_eq!(p["session_id"], "s1");
        let p = event_payload(apple::SessionEvent::Error("boom".into()), "s2");
        assert_eq!(p["session_id"], "s2");
        // 空 session 走旧全局通道：不带 session_id 键
        let p = event_payload(apple::SessionEvent::Final("完".into()), "");
        assert!(p.get("session_id").is_none());
    }
}
