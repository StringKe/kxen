//! goal 完成的 score-based 逐条验证：完成判据逐条过评审模型，全过才允许 complete。
//! 评审模型优先 review 角色绑定（独立视角），未配置回落当前会话模型（自证弱于独立评审，但零配置可用）。
//! 评审调用失败/输出不可解析按「本次 complete 拒绝」处理（可重试），不降级为弱校验静默放行。

use crate::llm::{Message, ModelRef};

const EVIDENCE_CAP: usize = 8000;
pub const JUDGE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq)]
pub struct CriterionScore {
    pub criterion: String,
    pub pass: bool,
    pub reason: String,
}

pub struct CompletionAttempt {
    pub result: Result<Vec<CriterionScore>, String>,
    /// True only after managed admission crossed the Provider boundary.
    pub request_started: bool,
    pub usage: Option<crate::llm::managed::TokenUsage>,
    pub unmetered_call: bool,
    pub metering_warning: Option<String>,
}

pub struct CompletionRequest<'a> {
    pub mrm: &'a crate::llm::mrm::ModelResourceManager,
    pub model: &'a ModelRef,
    pub store: &'a crate::auth::credential::AuthStore,
    pub objective: &'a str,
    pub criteria: &'a str,
    pub evidence: &'a str,
    pub timeout: std::time::Duration,
    pub cancel: Option<&'a crate::agent::cancel::CancelToken>,
    /// Provider 网络边界前的 durable Started 标记（completion 计量 claim），
    /// 在 permit.start() 之前 fsync；admission 失败/取消仍按 Prepared 丢弃。
    pub start_barrier: Option<Box<dyn FnMut() -> Result<(), String> + Send + 'a>>,
}

/// 判据文本拆条：非空行剥列表前缀（- / * / 1. / 1) / - [ ]）。
pub fn split_criteria(criteria: &str) -> Vec<String> {
    criteria
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| {
            let l = l.trim_start_matches("- [ ]").trim_start_matches("- [x]").trim_start_matches("- [X]");
            let l = l.trim_start_matches(['-', '*']).trim_start();
            // 数字有序前缀：1. / 2)
            let l = match l.find(['.', ')']) {
                Some(i) if i <= 3 && l[..i].chars().all(|c| c.is_ascii_digit()) && !l[..i].is_empty() => l[i + 1..].trim_start(),
                _ => l,
            };
            l.to_string()
        })
        .filter(|l| !l.is_empty())
        .collect()
}

/// Criterion identity comes only from the local contract. The model returns
/// one-based indices; duplicates, omissions, and reordering fail closed.
pub fn parse_scores(text: &str, criteria: &[String]) -> Option<Vec<CriterionScore>> {
    let start = text.find('[')?;
    let end = text.rfind(']')?;
    if end <= start {
        return None;
    }
    #[derive(serde::Deserialize)]
    struct Raw {
        index: usize,
        pass: bool,
        #[serde(default)]
        reason: String,
    }
    let raw: Vec<Raw> = serde_json::from_str(&text[start..=end]).ok()?;
    if raw.is_empty() {
        return None;
    }
    if raw.len() != criteria.len() || raw.iter().enumerate().any(|(offset, score)| score.index != offset + 1) {
        return None;
    }
    Some(
        raw.into_iter()
            .zip(criteria)
            .map(|(score, criterion)| CriterionScore { criterion: criterion.clone(), pass: score.pass, reason: score.reason })
            .collect(),
    )
}

/// 逐条评审：每条判据一个 pass/reason，条数必须与判据数一致（漏条 = 评审不可信，按失败重试）。
pub async fn score_completion(request: CompletionRequest<'_>) -> CompletionAttempt {
    let CompletionRequest { mrm, model, store, objective, criteria, evidence, timeout, cancel, start_barrier } = request;
    let items = split_criteria(criteria);
    if items.is_empty() {
        return CompletionAttempt {
            result: Err("completion_criteria 拆不出判据条目，无法逐条验证".into()),
            request_started: false,
            usage: None,
            unmetered_call: false,
            metering_warning: None,
        };
    }
    let evidence_capped: String = evidence.chars().take(EVIDENCE_CAP).collect();
    let numbered = items.iter().enumerate().map(|(i, c)| format!("{}. {}", i + 1, c)).collect::<Vec<_>>().join("\n");
    let messages = vec![
        Message::system(
            "You are a strict completion verifier for a coding agent's goal. \
             Score each completion criterion against the evidence. \
             Reply with ONLY a JSON array, one object per criterion in the same order, \
             using each one-based index exactly once: \
             [{\"index\": 1, \"pass\": true, \"reason\": \"...\"}]. \
             pass=true only when the evidence concretely demonstrates the criterion \
             (commands actually run with shown output, files actually changed, tests actually green). \
             Vague claims, intentions, and partial results must fail.",
        ),
        Message::user(format!("Objective: {objective}\n\nCompletion criteria:\n{numbered}\n\nEvidence:\n{evidence_capped}")),
    ];
    match crate::llm::managed::collect_text_observed_with_policy_and_start(
        mrm,
        model,
        &messages,
        store,
        timeout,
        None,
        cancel,
        crate::llm::managed::CircuitPolicy::Record,
        start_barrier,
    )
    .await
    {
        Ok(output) => {
            let result = parse_scores(&output.text, &items)
                .ok_or_else(|| "completion verification returned unparseable scores".to_string())
                .and_then(|scores| {
                    if scores.len() == items.len() {
                        Ok(scores)
                    } else {
                        Err(format!("completion verification scored {}/{} criteria, retry", scores.len(), items.len()))
                    }
                });
            CompletionAttempt {
                result,
                request_started: true,
                usage: output.usage.clone(),
                unmetered_call: output.usage.is_none(),
                metering_warning: output.metering_warning,
            }
        }
        Err(error) => CompletionAttempt {
            result: Err(error.message),
            request_started: error.request_started,
            usage: error.usage,
            unmetered_call: error.request_started && !error.usage_reported,
            metering_warning: error.metering_warning,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_criteria_strips_list_prefixes() {
        let c = "- cargo test 全绿\n* dmg < 20MB\n1. 文档更新\n2) 无警告\n- [ ] 可选项\n裸行判据";
        let items = split_criteria(c);
        assert_eq!(items, vec!["cargo test 全绿", "dmg < 20MB", "文档更新", "无警告", "可选项", "裸行判据"]);
    }

    #[test]
    fn parse_scores_tolerates_prose_wrapper() {
        let text = "以下是评审结果：\n[{\"index\":1,\"pass\":true,\"reason\":\"ok\"},{\"index\":2,\"pass\":false}]\n以上。";
        let scores = parse_scores(text, &["a".into(), "b".into()]).expect("应解析成功");
        assert_eq!(scores.len(), 2);
        assert!(scores[0].pass);
        assert!(!scores[1].pass);
        assert_eq!(scores[1].reason, "");
    }

    #[test]
    fn parse_scores_rejects_garbage() {
        let criteria = vec!["a".into(), "b".into()];
        assert!(parse_scores("没有 JSON", &criteria).is_none());
        assert!(parse_scores("[]", &criteria).is_none());
        assert!(parse_scores("[{\"index\":1}]", &criteria).is_none());
        assert!(parse_scores("[{\"index\":1,\"pass\":true},{\"index\":1,\"pass\":true}]", &criteria).is_none());
        assert!(parse_scores("[{\"index\":2,\"pass\":true},{\"index\":1,\"pass\":true}]", &criteria).is_none());
        assert!(parse_scores("[{\"index\":1,\"pass\":true}]", &criteria).is_none());
    }

    #[tokio::test]
    async fn empty_criteria_is_rejected_before_provider_admission() {
        let mrm = crate::llm::mrm::ModelResourceManager::new(Default::default());
        let model = ModelRef::new("unused", "unused");
        let store = crate::auth::credential::AuthStore::default();
        let attempt = score_completion(CompletionRequest {
            mrm: &mrm,
            model: &model,
            store: &store,
            objective: "objective",
            criteria: " \n ",
            evidence: "evidence",
            timeout: JUDGE_TIMEOUT,
            cancel: None,
            start_barrier: None,
        })
        .await;
        assert!(!attempt.request_started);
        assert!(attempt.usage.is_none());
        assert!(!attempt.unmetered_call);
    }
}
