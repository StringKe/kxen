use super::usage_totals;

#[test]
fn usage_overview_totals_keep_unmetered_calls_in_completeness() {
    let tokens = [
        ("s1".to_string(), kxen_app::core::usage::SessionUsage { input: 100, output: 20, ..Default::default() }),
        ("s2".to_string(), kxen_app::core::usage::SessionUsage { input: 7, output: 3, unmetered_calls: 2, ..Default::default() }),
        (
            "system_provider_verify".to_string(),
            kxen_app::core::usage::SessionUsage { input: 1, output: 1, unmetered_calls: 1, ..Default::default() },
        ),
    ]
    .into_iter()
    .collect();

    let totals = usage_totals(&tokens);
    assert_eq!(totals.total_input, 108);
    assert_eq!(totals.total_output, 24);
    assert_eq!(totals.unmetered_calls, 3);
    assert_eq!(totals.sessions, 2, "synthetic usage scopes must not inflate chat Session count");
    assert!(!totals.completeness.usage_complete);
}
