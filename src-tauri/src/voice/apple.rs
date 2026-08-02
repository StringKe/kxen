//! Apple 原生语音引擎（Speech.framework，本地离线识别 zh/en）。

use super::EngineStatus;
use super::objc;

pub fn status() -> EngineStatus {
    if let Err(detail) = objc::availability() {
        return EngineStatus { id: "apple".into(), label: "Apple 本地识别".into(), status: "unavailable".into(), detail };
    }
    let (status, detail) = match objc::authorization_status() {
        objc::SpeechAuth::Authorized => ("ready", "Speech.framework 已授权"),
        objc::SpeechAuth::NotDetermined => ("needs_auth", "首次使用将请求语音识别权限"),
        _ => ("unavailable", "语音识别权限被拒绝/受限，请在系统设置开启"),
    };
    EngineStatus { id: "apple".into(), label: "Apple 本地识别".into(), status: status.into(), detail: detail.into() }
}

fn ensure_authorized() -> Result<(), String> {
    objc::availability()?;
    match objc::authorization_status() {
        objc::SpeechAuth::Authorized => Ok(()),
        objc::SpeechAuth::NotDetermined => {
            let (tx, rx) = std::sync::mpsc::channel();
            objc::request_authorization(move |s| {
                let _ = tx.send(s);
            });
            let s = rx.recv_timeout(std::time::Duration::from_secs(60)).map_err(|_| "授权等待超时".to_string())?;
            if s == objc::SpeechAuth::Authorized { Ok(()) } else { Err("语音识别权限未授予".into()) }
        }
        _ => Err("语音识别权限被拒绝/受限".into()),
    }
}

/// 创建识别器并确认可用 + 支持 on-device；不支持则报错，由 voice::start 的 fallback 链降级到云转写。
fn on_device_recognizer(locale: &str) -> Result<Retained<AnyObject>, String> {
    let recognizer = objc::new_recognizer(locale).ok_or_else(|| format!("无法创建识别器（locale {locale}）"))?;
    if !objc::is_available(&recognizer) {
        return Err("识别服务当前不可用".into());
    }
    if !objc::supports_on_device(&recognizer) {
        tracing::warn!(locale, "Apple 识别器不支持 on-device 识别，降级到云转写链");
        return Err(format!("识别器不支持 on-device 识别（locale {locale}）"));
    }
    Ok(recognizer)
}

#[cfg(test)]
mod tests {
    #[test]
    fn status_renders() {
        let s = super::status();
        assert_eq!(s.id, "apple");
        assert!(["ready", "needs_auth", "unavailable"].contains(&s.status.as_str()));
    }

    #[test]
    fn availability_is_explicit_result_not_panic() {
        // 标准 macOS 上框架齐全应 Ok；缺失时须是带 detail 的 Err（引擎标 unavailable），不得 panic
        match super::objc::availability() {
            Ok(()) => {}
            Err(detail) => assert!(detail.contains("ObjC"), "detail 须指明缺失类: {detail}"),
        }
    }
}

// ---------------- 麦克风流式会话 ----------------

use objc2::rc::Retained;
use objc2::runtime::AnyObject;

#[derive(Debug)]
pub enum SessionEvent {
    Partial(String),
    Final(String),
    Error(String),
}

pub struct MicSession {
    task: Retained<AnyObject>,
    engine: Retained<AnyObject>,
    request: Retained<AnyObject>,
    /// tap block 与 tap 回调持有的 request retain：stop/cancel 先 removeTap 再随结构体回收
    /// （旧实现 mem::forget 每次 PTT 各泄漏一份）。
    tap: objc::TapHandler,
    req_kept: Retained<AnyObject>,
    rx: std::sync::mpsc::Receiver<SessionEvent>,
    samples: std::sync::Arc<std::sync::Mutex<Vec<f32>>>,
    sample_rate: u32,
}

/// 启动麦克风识别（PTT 按下）。tap 同时喂 Speech（本地流式）与 PCM 缓冲（云转写终稿用）。
pub fn start_mic(locale: &str) -> Result<MicSession, String> {
    ensure_authorized()?;
    let recognizer = on_device_recognizer(locale)?;
    let request = objc::buffer_request().ok_or("无法创建缓冲识别请求")?;
    let (tx, rx) = std::sync::mpsc::channel::<SessionEvent>();
    let handler = objc::ResultHandler::new(move |result, error| {
        if let Some(e) = objc::error_text(error) {
            let _ = tx.send(SessionEvent::Error(e));
            return;
        }
        if let Some((text, is_final)) = unsafe { objc::result_text(result) } {
            let _ = tx.send(if is_final { SessionEvent::Final(text) } else { SessionEvent::Partial(text) });
        }
    });
    let task = objc::recognition_task(&recognizer, &request, &handler).ok_or("无法启动识别任务")?;
    // tap 线程经裸指针读 request：显式 retain 一份防悬垂，session 结束（removeTap 之后）随结构体回收
    let req_ptr = &*request as *const AnyObject as *mut AnyObject;
    let req_kept = unsafe { objc::retain_autoreleased(req_ptr) }.ok_or("request 持有失败")?;
    let samples: std::sync::Arc<std::sync::Mutex<Vec<f32>>> = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = samples.clone();
    let (engine, rate, tap) = objc::start_mic_capture(move |_input| {
        objc::TapHandler::new(move |buffer: *mut AnyObject, _time: *mut AnyObject| {
            if !buffer.is_null() {
                objc::append_buffer(unsafe { &*req_ptr }, buffer);
                let chunk = unsafe { objc::pcm_samples(buffer) };
                if !chunk.is_empty() {
                    crate::core::shared::lock(&sink).extend_from_slice(&chunk);
                }
            }
        })
    })?;
    Ok(MicSession { task, engine, request, tap, req_kept, rx, samples, sample_rate: rate as u32 })
}

impl MicSession {
    /// 非阻塞排空已到事件（泵给前端）。
    pub fn drain(&self) -> Vec<SessionEvent> {
        let mut out = Vec::new();
        while let Ok(e) = self.rx.try_recv() {
            out.push(e);
        }
        out
    }

    /// PTT 松开：停止采集 -> endAudio -> 等 final（3s 兜底）-> cancel。
    /// 返回 (本地终稿, 云转写用 WAV 路径)。
    pub fn stop(self) -> (Option<String>, Option<String>) {
        objc::stop_mic_engine(&self.engine);
        // removeTap 之后回收 tap block 与 request retain（旧实现 mem::forget 每次 PTT 各泄漏一份）
        drop(self.tap);
        drop(self.req_kept);
        objc::end_audio(&self.request);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        let mut last: Option<String> = None;
        loop {
            let remain = deadline.saturating_duration_since(std::time::Instant::now());
            if remain.is_zero() {
                break;
            }
            match self.rx.recv_timeout(remain) {
                Ok(SessionEvent::Final(t)) => {
                    last = Some(t);
                    break;
                }
                Ok(SessionEvent::Partial(t)) => last = Some(t),
                Ok(SessionEvent::Error(_)) | Err(_) => break,
            }
        }
        objc::cancel_task(&self.task);
        let wav = {
            let samples = crate::core::shared::lock(&self.samples);
            if samples.is_empty() {
                None
            } else {
                let path = super::provider::temp_wav_path();
                super::provider::write_wav_pub(&path, &samples, self.sample_rate).ok().map(|_| path.to_string_lossy().into_owned())
            }
        };
        (last, wav)
    }

    /// Session 被删除或同 Session 重启 PTT 时立即释放麦克风，不等待终稿。
    pub fn cancel(self) {
        objc::stop_mic_engine(&self.engine);
        drop(self.tap);
        drop(self.req_kept);
        objc::end_audio(&self.request);
        objc::cancel_task(&self.task);
    }
}
