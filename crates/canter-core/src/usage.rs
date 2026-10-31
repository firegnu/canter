//! Server-side pricing and replay of the durable per-request ledger.
use std::collections::BTreeMap;

use canter_protocol::*;
use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
pub struct Prices {
    pub cache_hit: f64,
    pub cache_miss: f64,
    pub output: f64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ModelPrice {
    pub peak: Prices,
    pub off_peak: Option<Prices>,
}

pub type PriceBook = BTreeMap<String, ModelPrice>;

pub fn builtin_prices() -> PriceBook {
    // USD per million tokens, retrieved 2026-10-25:
    // https://api-docs.deepseek.com/quick_start/pricing/
    // Chinese public holidays follow the weekday schedule (may overestimate).
    [
        ("deepseek-flash", [0.006, 0.30, 1.20], [0.003, 0.15, 0.60]),
        ("deepseek-v4-pro", [0.044, 1.32, 3.96], [0.022, 0.66, 1.98]),
    ]
    .into_iter()
    .map(|(model, peak, off_peak)| {
        let price = |[cache_hit, cache_miss, output]: [f64; 3]| Prices {
            cache_hit,
            cache_miss,
            output,
        };
        (
            model.into(),
            ModelPrice {
                peak: price(peak),
                off_peak: Some(price(off_peak)),
            },
        )
    })
    .collect()
}

impl ModelPrice {
    pub fn valid(&self) -> bool {
        std::iter::once(&self.peak)
            .chain(self.off_peak.iter())
            .all(|p| {
                [p.cache_hit, p.cache_miss, p.output]
                    .into_iter()
                    .all(|v| v.is_finite() && v >= 0.0)
            })
    }

    pub fn charge(&self, started_at: i64, tokens: &TokenUsageBreakdown) -> f64 {
        // Unix epoch was Thursday; days and hours are UTC, including negative timestamps.
        let seconds = started_at.div_euclid(1000);
        let weekday = (seconds.div_euclid(86_400) + 3).rem_euclid(7);
        let hour = seconds.rem_euclid(86_400) / 3600;
        let peak = weekday < 5 && ((1..4).contains(&hour) || (6..10).contains(&hour));
        let price = if peak {
            &self.peak
        } else {
            self.off_peak.as_ref().unwrap_or(&self.peak)
        };
        (tokens.cached_input_tokens as f64 * price.cache_hit
            + tokens
                .input_tokens
                .saturating_sub(tokens.cached_input_tokens) as f64
                * price.cache_miss
            + tokens.output_tokens as f64 * price.output)
            / 1_000_000.0
    }
}

pub fn request_charge(
    prices: &PriceBook,
    model: &str,
    started_at: i64,
    tokens: &TokenUsageBreakdown,
) -> RequestCharge {
    RequestCharge {
        request_id: None,
        model: model.into(),
        started_at,
        cost_usd: prices
            .get(model)
            .map(|price| price.charge(started_at, tokens)),
    }
}

pub fn cost(charge: &RequestCharge) -> UsageCost {
    UsageCost {
        usd: charge.cost_usd.unwrap_or(0.0),
        unknown: charge.cost_usd.is_none(),
    }
}

#[derive(Default)]
pub struct Ledger {
    /// Event timestamps, used for local-day grouping even for new requests.
    pub records: Vec<(i64, UsageRow)>,
    pub report: UsageResponse,
    turn_costs: BTreeMap<String, UsageCost>,
}

impl Ledger {
    pub fn replay(events: &[Event], prices: &PriceBook) -> Self {
        let mut ledger = Self::default();
        let mut model = String::new();
        let mut turn_id = None;
        let mut models = BTreeMap::<String, UsageRow>::new();
        for event in events {
            match &event.payload {
                EventPayload::SessionUpdated { settings } => model.clone_from(&settings.model),
                EventPayload::TurnStarted { turn, .. } => turn_id = Some(turn.id.clone()),
                EventPayload::TurnCompleted { turn } => {
                    ledger.report.last_turn_cost =
                        Some(ledger.turn_costs.get(&turn.id).cloned().unwrap_or_default());
                    turn_id = None;
                }
                EventPayload::SessionUsage { token_usage }
                | EventPayload::SessionBtwUsage { token_usage } => {
                    let charge = token_usage.request.clone().unwrap_or_else(|| {
                        request_charge(prices, &model, event.timestamp, &token_usage.last)
                    });
                    let row = UsageRow {
                        model: Some(charge.model.clone()),
                        cached_input_tokens: token_usage.last.cached_input_tokens,
                        uncached_input_tokens: token_usage
                            .last
                            .input_tokens
                            .saturating_sub(token_usage.last.cached_input_tokens),
                        output_tokens: token_usage.last.output_tokens,
                        cost: cost(&charge),
                        ..UsageRow::default()
                    };
                    if let Some(id) = &turn_id
                        && matches!(event.payload, EventPayload::SessionUsage { .. })
                    {
                        ledger
                            .turn_costs
                            .entry(id.clone())
                            .or_default()
                            .add(&row.cost);
                    }
                    let group = models.entry(charge.model).or_insert_with(|| UsageRow {
                        model: row.model.clone(),
                        ..UsageRow::default()
                    });
                    group.add(&row);
                    ledger.report.total.add(&row);
                    ledger.records.push((event.timestamp, row));
                }
                _ => {}
            }
        }
        ledger.report.data = models.into_values().collect();
        ledger
    }

    /// Enrich an in-memory snapshot only; the stored events remain untouched.
    pub fn fill_costs(&self, snapshot: &mut SessionSnapshot) {
        if let Some(usage) = &mut snapshot.token_usage {
            usage.cost = Some(self.report.total.cost.clone());
        }
        for turn in &mut snapshot.turns {
            if let Some(usage) = &mut turn.usage {
                usage.cost = Some(self.turn_costs.get(&turn.id).cloned().unwrap_or_default());
            }
        }
    }
}
