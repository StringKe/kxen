//! 按本地日期和 Provider 持久化 token 趋势。金额只用用户配置的真实单价计算，
//! 订阅或未知定价保持 UNKNOWN，不能用静态公开价冒充实际账单。

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProviderUsage {
    pub input: u64,
    pub output: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DayUsage {
    pub input: u64,
    pub output: u64,
    pub by_provider: BTreeMap<String, ProviderUsage>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Ledger {
    days: BTreeMap<String, DayUsage>,
}

fn store_file() -> PathBuf {
    crate::core::paths::data_dir().join("usage-trend.json")
}

fn io_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn today_key() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

fn load_from(path: &Path) -> Ledger {
    std::fs::read_to_string(path).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default()
}

fn persist_to(path: &Path, ledger: &Ledger) {
    let Ok(json) = serde_json::to_string_pretty(ledger) else { return };
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, json).is_ok() {
        let _ = std::fs::rename(tmp, path);
    }
}

fn record_to(path: &Path, date: &str, provider: &str, input: u64, output: u64) {
    if input == 0 && output == 0 {
        return;
    }
    let mut ledger = load_from(path);
    let day = ledger.days.entry(date.to_string()).or_default();
    day.input = day.input.saturating_add(input);
    day.output = day.output.saturating_add(output);
    let provider = day.by_provider.entry(provider.to_string()).or_default();
    provider.input = provider.input.saturating_add(input);
    provider.output = provider.output.saturating_add(output);
    while ledger.days.len() > 90 {
        let Some(first) = ledger.days.keys().next().cloned() else { break };
        ledger.days.remove(&first);
    }
    persist_to(path, &ledger);
}

pub fn record(provider: &str, input: u64, output: u64) {
    let _guard = io_lock().lock().expect("usage trend lock");
    record_to(&store_file(), &today_key(), provider, input, output);
}

fn day_from(path: &Path, date: &str) -> DayUsage {
    load_from(path).days.remove(date).unwrap_or_default()
}

pub fn today() -> DayUsage {
    let _guard = io_lock().lock().expect("usage trend lock");
    day_from(&store_file(), &today_key())
}

fn recent_from(path: &Path, days: usize) -> Vec<(String, DayUsage)> {
    let ledger = load_from(path);
    let mut out: Vec<_> = ledger.days.into_iter().rev().take(days).collect();
    out.reverse();
    out
}

pub fn recent(days: usize) -> Vec<(String, DayUsage)> {
    let _guard = io_lock().lock().expect("usage trend lock");
    recent_from(&store_file(), days)
}

pub fn provider_cost_usd(usage: &ProviderUsage, limit: &crate::core::config::ProviderLimit) -> Option<f64> {
    let input = limit.input_usd_per_million?;
    let output = limit.output_usd_per_million?;
    if !input.is_finite() || !output.is_finite() || input < 0.0 || output < 0.0 {
        return None;
    }
    Some((usage.input as f64 * input + usage.output as f64 * output) / 1_000_000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_cost_use_explicit_rates() {
        let path = std::env::temp_dir().join(format!("kxen-usage-trend-{}.json", std::process::id()));
        let mut ledger = Ledger::default();
        ledger.days.insert(
            "2026-07-28".into(),
            DayUsage {
                input: 1_000_000,
                output: 500_000,
                by_provider: BTreeMap::from([("p".into(), ProviderUsage { input: 1_000_000, output: 500_000 })]),
            },
        );
        persist_to(&path, &ledger);
        assert_eq!(load_from(&path).days["2026-07-28"].output, 500_000);
        let limit = crate::core::config::ProviderLimit {
            input_usd_per_million: Some(2.0),
            output_usd_per_million: Some(4.0),
            ..Default::default()
        };
        assert_eq!(provider_cost_usd(&ledger.days["2026-07-28"].by_provider["p"], &limit), Some(4.0));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn unknown_rates_do_not_invent_cost() {
        assert_eq!(provider_cost_usd(&ProviderUsage { input: 1, output: 1 }, &Default::default()), None);
    }

    #[test]
    fn record_and_query_preserve_provider_totals_and_date_order() {
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("system time").as_nanos();
        let path = std::env::temp_dir().join(format!("kxen-usage-trend-public-{nonce}.json"));

        record_to(&path, "2026-07-27", "openai", 0, 0);
        assert!(!path.exists());

        record_to(&path, "2026-07-27", "openai", 10, 3);
        record_to(&path, "2026-07-27", "openai", 5, 2);
        record_to(&path, "2026-07-28", "anthropic", 7, 4);

        let first = day_from(&path, "2026-07-27");
        assert_eq!((first.input, first.output), (15, 5));
        assert_eq!((first.by_provider["openai"].input, first.by_provider["openai"].output), (15, 5));
        assert_eq!(day_from(&path, "missing").input, 0);

        let recent = recent_from(&path, 2);
        assert_eq!(recent.iter().map(|(date, _)| date.as_str()).collect::<Vec<_>>(), ["2026-07-27", "2026-07-28"]);
        assert_eq!(recent[1].1.by_provider["anthropic"].output, 4);
        assert_eq!(recent_from(&path, 1)[0].0, "2026-07-28");
        let _ = std::fs::remove_file(path);
    }
}
