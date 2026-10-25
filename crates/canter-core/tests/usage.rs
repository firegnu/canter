use canter_core::usage::{ModelPrice, builtin_prices};
use canter_protocol::TokenUsageBreakdown;

#[test]
fn utc_boundaries_weekends_and_single_price_configuration() {
    let prices = builtin_prices();
    let flat: ModelPrice =
        serde_json::from_str(r#"{"peak":{"cache_hit":1,"cache_miss":2,"output":3}}"#).unwrap();
    let tokens = TokenUsageBreakdown {
        input_tokens: 1_000_000,
        cached_input_tokens: 800_000,
        output_tokens: 100_000,
        reasoning_output_tokens: 50_000,
        total_tokens: 1_100_000,
    };
    // Monday 1970-01-05 UTC, half-open peak intervals; every hour of the weekend.
    for (second, peak) in [
        (3599, false),
        (3600, true),
        (14399, true),
        (14400, false),
        (21599, false),
        (21600, true),
        (35999, true),
        (36000, false),
    ] {
        let at = (345_600 + second) * 1000;
        let expected = if peak { 0.1848 } else { 0.0924 };
        assert!((prices["deepseek-flash"].charge(at, &tokens) - expected).abs() < 1e-10);
        assert!((flat.charge(at, &tokens) - 1.5).abs() < 1e-10);
    }
    for day in [9, 10] {
        // Saturday, Sunday
        for hour in 0..24 {
            let at = (day * 86_400 + hour * 3600) * 1000;
            assert!((prices["deepseek-flash"].charge(at, &tokens) - 0.0924).abs() < 1e-10);
            assert!((flat.charge(at, &tokens) - 1.5).abs() < 1e-10);
        }
    }
}
