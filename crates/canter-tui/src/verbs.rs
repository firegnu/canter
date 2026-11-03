//! The playful word on the status line while a turn works (P2-199): one
//! for each turn, kept for the whole turn, never the previous turn's word.

use canter_protocol::Turn;

/// Our own list; a few are a horse's gaits, as `canter` is one.
const WORDS: [&str; 38] = [
    "Cantering…",
    "Trotting…",
    "Galloping…",
    "Prancing…",
    "Grazing…",
    "Whinnying…",
    "Nickering…",
    "Ambling…",
    "Tinkering…",
    "Fiddling…",
    "Rummaging…",
    "Untangling…",
    "Sleuthing…",
    "Scheming…",
    "Doodling…",
    "Juggling…",
    "Whittling…",
    "Kneading…",
    "Sifting…",
    "Wrangling…",
    "Puzzling…",
    "Daydreaming…",
    "Rustling…",
    "Burrowing…",
    "Polishing…",
    "Sketching…",
    "Plotting…",
    "Zigzagging…",
    "Bumbling…",
    "Foraging…",
    "Spelunking…",
    "Hopscotching…",
    "Waltzing…",
    "Galumphing…",
    "Meandering…",
    "Tumbling…",
    "Gallivanting…",
    "Moonwalking…",
];

/// The word for the last of `turns`. Turn ids come from the server and are
/// random, so a hash of the id is a random pick that stays the same for
/// the turn and on every client; a pick equal to the turn before's moves on
/// to the next word.
pub(crate) fn word(turns: &[Turn]) -> &'static str {
    let mut previous = None;
    let mut word = WORDS[0];
    for turn in turns {
        let mut at = (hash(&turn.id) % WORDS.len() as u64) as usize;
        if previous == Some(at) {
            at = (at + 1) % WORDS.len();
        }
        previous = Some(at);
        word = WORDS[at];
    }
    word
}

/// FNV-1a: the same on every build and platform.
fn hash(id: &str) -> u64 {
    id.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turns(ids: &[&str]) -> Vec<Turn> {
        ids.iter()
            .map(|id| {
                serde_json::from_value(serde_json::json!({
                    "id": id, "items": [], "status": "completed", "error": null,
                    "input": [], "clientName": "canter-tui", "usage": null,
                }))
                .unwrap()
            })
            .collect()
    }

    #[test]
    fn a_turn_keeps_its_word_and_the_next_turn_never_repeats_it() {
        let ids: Vec<String> = (0..500).map(|n| format!("turn-{n}")).collect();
        let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
        let all = turns(&ids);
        let mut seen = std::collections::HashSet::new();
        for end in 1..all.len() {
            let before = word(&all[..end]);
            let now = word(&all[..=end]);
            assert_ne!(before, now, "turn {end}");
            assert_eq!(word(&all[..=end]), now);
            seen.insert(now);
        }
        // Spread over the list, not stuck on a few words.
        assert!(seen.len() > WORDS.len() / 2, "{}", seen.len());
        assert!(WORDS.iter().all(|word| word.ends_with('…')));
    }
}
