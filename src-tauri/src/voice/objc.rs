//! Speech.framework / AVFAudio 手写绑定（官方无 objc2-speech crate，最小可用面）。

use block2::RcBlock;
use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool};
use objc2_foundation::{NSError, NSLocale, NSString};

#[link(name = "Speech", kind = "framework")]
unsafe extern "C" {}

#[link(name = "AVFAudio", kind = "framework")]
unsafe extern "C" {}

fn class(name: &std::ffi::CStr) -> Option<&'static AnyClass> {
    AnyClass::get(name)
}

/// 框架探测：Speech/AVFAudio 类缺失（框架被裁的非标准系统）时引擎必须降级报错，
/// 不能 panic 拖垮进程（fail-fast 语义保留：调用方拿到 Err 即标记引擎 unavailable）。
pub fn availability() -> Result<(), String> {
    for c in [c"SFSpeechRecognizer", c"SFSpeechAudioBufferRecognitionRequest", c"AVAudioEngine"] {
        if class(c).is_none() {
            return Err(format!("系统缺少 ObjC 类 {c:?}（Speech/AVFAudio 框架不可用）"));
        }
    }
    Ok(())
}

/// +0（autoreleased）返回的安全持有：显式 retain 后交接所有权。
///
/// # Safety
/// `ptr` 必须是有效的 Objective-C 对象指针或可空；非对象指针属调用方契约违反。
pub unsafe fn retain_autoreleased(ptr: *mut AnyObject) -> Option<Retained<AnyObject>> {
    if ptr.is_null() {
        return None;
    }
    unsafe {
        let _: *mut AnyObject = msg_send![ptr, retain];
        Retained::from_raw(ptr)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpeechAuth {
    Authorized,
    Denied,
    Restricted,
    NotDetermined,
}

impl SpeechAuth {
    fn of(status: isize) -> Self {
        match status {
            0 => Self::Authorized,
            1 => Self::Denied,
            2 => Self::Restricted,
            _ => Self::NotDetermined,
        }
    }
}

/// 当前识别授权状态（同步，不触发弹窗）。
pub fn authorization_status() -> SpeechAuth {
    // 入口已经 availability() 门禁；这里的兜底只是防御（类缺失按受限处理 = unavailable）
    let Some(cls) = class(c"SFSpeechRecognizer") else {
        return SpeechAuth::Restricted;
    };
    let status: isize = unsafe { msg_send![cls, authorizationStatus] };
    SpeechAuth::of(status)
}

/// 请求识别授权（异步回调；未决时系统弹 TCC 窗）。
pub fn request_authorization(cb: impl Fn(SpeechAuth) + Send + Sync + 'static) {
    let Some(cls) = class(c"SFSpeechRecognizer") else {
        cb(SpeechAuth::Restricted);
        return;
    };
    let block = RcBlock::new(move |status: isize| cb(SpeechAuth::of(status)));
    let _: () = unsafe { msg_send![cls, requestAuthorization: &*block] };
}

/// 创建指定 locale 的识别器（如 zh-CN / en-US）。
pub fn new_recognizer(locale_id: &str) -> Option<Retained<AnyObject>> {
    let cls = class(c"SFSpeechRecognizer")?;
    unsafe {
        let locale = NSLocale::localeWithLocaleIdentifier(&NSString::from_str(locale_id));
        let obj: *mut AnyObject = msg_send![cls, alloc];
        let obj: *mut AnyObject = msg_send![obj, initWithLocale: &*locale];
        Retained::from_raw(obj)
    }
}

/// 识别器当前可用（网络/服务可达；on-device 不保证）。
pub fn is_available(recognizer: &AnyObject) -> bool {
    let v: Bool = unsafe { msg_send![recognizer, isAvailable] };
    v.as_bool()
}

/// 识别器是否支持纯本地识别（SFSpeechRecognizer.supportsOnDeviceRecognition）。
/// 不支持时必须走降级链：请求已强制 setRequiresOnDeviceRecognition:YES，硬跑只会报错。
pub fn supports_on_device(recognizer: &AnyObject) -> bool {
    let v: Bool = unsafe { msg_send![recognizer, supportsOnDeviceRecognition] };
    v.as_bool()
}

pub type ResultHandler = RcBlock<dyn Fn(*mut AnyObject, *mut AnyObject)>;

/// 流式缓冲识别请求（麦克风灌流专用；强制本地识别）。
pub fn buffer_request() -> Option<Retained<AnyObject>> {
    let cls = class(c"SFSpeechAudioBufferRecognitionRequest")?;
    unsafe {
        let req: *mut AnyObject = msg_send![cls, alloc];
        let req: *mut AnyObject = msg_send![req, init];
        let req = Retained::from_raw(req)?;
        let _: () = msg_send![&*req, setShouldReportPartialResults: Bool::YES];
        let _: () = msg_send![&*req, setRequiresOnDeviceRecognition: Bool::YES];
        Some(req)
    }
}

/// 追加一帧 PCM（AVAudioPCMBuffer）。
pub fn append_buffer(request: &AnyObject, buffer: *mut AnyObject) {
    let _: () = unsafe { msg_send![request, appendAudioPCMBuffer: buffer] };
}

/// 灌流结束（触发 final 结果）。
pub fn end_audio(request: &AnyObject) {
    let _: () = unsafe { msg_send![request, endAudio] };
}

pub type TapHandler = RcBlock<dyn Fn(*mut AnyObject, *mut AnyObject)>;

/// AVAudioEngine 麦克风采集（通用）：tap 回调直送 caller 的 handler（Speech 灌流 / PCM 累积两吃）。
/// 返回 (engine, 采样率, tap block)。tap 由调用方持有到 stop_mic_engine（removeTap）之后回收：
/// installTap 内部 Block_copy，但回调跑在音频实时线程，Rust 侧保活一份最稳；
/// 旧实现 mem::forget 每次 PTT 泄漏一份 block。
pub fn start_mic_capture(
    make_handler: impl FnOnce(*mut AnyObject) -> TapHandler,
) -> Result<(Retained<AnyObject>, f64, TapHandler), String> {
    unsafe {
        let Some(cls) = class(c"AVAudioEngine") else {
            return Err("系统缺少 ObjC 类 AVAudioEngine（AVFAudio 框架不可用）".into());
        };
        let engine: *mut AnyObject = msg_send![cls, alloc];
        let engine: *mut AnyObject = msg_send![engine, init];
        let engine = Retained::from_raw(engine).ok_or("AVAudioEngine 创建失败")?;
        let input: *mut AnyObject = msg_send![&*engine, inputNode];
        if input.is_null() {
            return Err("无音频输入节点".into());
        }
        let format: *mut AnyObject = msg_send![input, outputFormatForBus: 0usize];
        if format.is_null() {
            return Err("无输入格式".into());
        }
        let sample_rate: f64 = msg_send![format, sampleRate];
        let tap = make_handler(input);
        let _: () = msg_send![input, installTapOnBus: 0usize, bufferSize: 1024u32, format: format, block: &*tap];
        let mut error: *mut NSError = std::ptr::null_mut();
        let ok: Bool = msg_send![&*engine, startAndReturnError: &mut error];
        if !ok.as_bool() {
            // 启动失败：removeTap 后 tap 随函数返回回收
            let _: () = msg_send![input, removeTapOnBus: 0usize];
            return Err(if error.is_null() {
                "音频引擎启动失败".into()
            } else {
                error_text(error as *mut AnyObject).unwrap_or_else(|| "音频引擎启动失败".into())
            });
        }
        Ok((engine, sample_rate, tap))
    }
}

/// 停止采集（removeTap + stop）。
pub fn stop_mic_engine(engine: &AnyObject) {
    unsafe {
        let input: *mut AnyObject = msg_send![engine, inputNode];
        if !input.is_null() {
            let _: () = msg_send![input, removeTapOnBus: 0usize];
        }
        let _: () = msg_send![engine, stop];
    }
}

/// 读取 PCM 帧数据（float32 交错首通道）-> 采样副本。
///
/// # Safety
/// `buffer` 必须是 AVAudioPCMBuffer 有效指针（tap 回调送达的对象）。
pub unsafe fn pcm_samples(buffer: *mut AnyObject) -> Vec<f32> {
    if buffer.is_null() {
        return Vec::new();
    }
    unsafe {
        let len: u32 = msg_send![buffer, frameLength];
        let channels: *mut *mut f32 = msg_send![buffer, floatChannelData];
        if channels.is_null() || (*channels).is_null() {
            return Vec::new();
        }
        std::slice::from_raw_parts(*channels, len as usize).to_vec()
    }
}

/// 启动识别任务（handler 收 SFSpeechRecognitionResult / NSError，均为可空）。
pub fn recognition_task(recognizer: &AnyObject, request: &AnyObject, handler: &ResultHandler) -> Option<Retained<AnyObject>> {
    unsafe {
        let task: *mut AnyObject = msg_send![recognizer, recognitionTaskWithRequest: request, resultHandler: &**handler];
        retain_autoreleased(task)
    }
}

/// 取消识别任务。
pub fn cancel_task(task: &AnyObject) {
    let _: () = unsafe { msg_send![task, cancel] };
}

/// 解析识别结果 -> (formattedString, isFinal)。
///
/// # Safety
/// `result` 必须是 SFSpeechRecognitionResult 有效指针或可空（识别回调送达的对象）。
pub unsafe fn result_text(result: *mut AnyObject) -> Option<(String, bool)> {
    if result.is_null() {
        return None;
    }
    unsafe {
        let is_final: Bool = msg_send![result, isFinal];
        let transcription: *mut AnyObject = msg_send![result, bestTranscription];
        let transcription = retain_autoreleased(transcription)?;
        let text: *mut NSString = msg_send![&*transcription, formattedString];
        let text = retain_autoreleased(text as *mut AnyObject)?;
        let text: &NSString = &*(&*text as *const AnyObject as *const NSString);
        Some((text.to_string(), is_final.as_bool()))
    }
}

/// 解析错误 -> localizedDescription。
pub fn error_text(error: *mut AnyObject) -> Option<String> {
    if error.is_null() {
        return None;
    }
    let err: &NSError = unsafe { &*(error as *const NSError) };
    Some(err.localizedDescription().to_string())
}
