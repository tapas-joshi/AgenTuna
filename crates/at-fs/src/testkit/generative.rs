//! Deterministic pseudo-random sequences for property-style tests.
//!
//! The workspace intentionally has no property-testing crate: this module is a seeded
//! xorshift64\* generator, so a failing case is replayed by passing the seed printed in the
//! assertion message. It never reads the clock, the environment or the filesystem.
//!
//! ```
//! # #[cfg(feature = "testkit")]
//! # fn main() {
//! use at_fs::testkit::generative::Rng;
//!
//! let mut rng = Rng::new(7);
//! assert_eq!(rng.below(1), 0);
//! assert!(!rng.name(4).is_empty());
//! # }
//! # #[cfg(not(feature = "testkit"))]
//! # fn main() {}
//! ```

/// A small deterministic generator: xorshift64\* with a fixed seed.
#[derive(Debug, Clone)]
pub struct Rng {
    state: u64,
}

impl Rng {
    /// Starts a sequence. `0` is remapped to a fixed non-zero constant.
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self {
            state: if seed == 0 {
                0x9E37_79B9_7F4A_7C15
            } else {
                seed
            },
        }
    }

    /// The next 64-bit output.
    #[must_use]
    pub fn next_u64(&mut self) -> u64 {
        let mut state = self.state;
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        self.state = state;
        state.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// A value in `0..bound`. `bound == 0` yields `0`.
    #[must_use]
    pub fn below(&mut self, bound: usize) -> usize {
        if bound == 0 {
            return 0;
        }
        (self.next_u64() % bound as u64) as usize
    }

    /// A value in `min..=max`.
    #[must_use]
    pub fn span(&mut self, min: usize, max: usize) -> usize {
        min + self.below(max.saturating_sub(min) + 1)
    }

    /// `true` once in `one_in` calls on average. `one_in == 0` is `true`.
    #[must_use]
    pub fn chance(&mut self, one_in: u32) -> bool {
        self.below(one_in as usize) == 0
    }

    /// One element of `values`. `values` must not be empty.
    #[must_use]
    pub fn pick<'a, T>(&mut self, values: &'a [T]) -> &'a T {
        &values[self.below(values.len())]
    }

    /// A lower-case ASCII name of `len` letters, safe as a JSON/TOML key.
    #[must_use]
    pub fn name(&mut self, len: usize) -> String {
        const LETTERS: &[u8] = b"abcdefghijklmnopqrstuvwxyz";
        (0..len)
            .map(|_| char::from(LETTERS[self.below(LETTERS.len())]))
            .collect()
    }

    /// A name with a letter first and letters, digits and `_` after: a valid env var or id.
    #[must_use]
    pub fn identifier(&mut self, len: usize) -> String {
        const TAIL: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789_";
        let mut text = self.name(1);
        for _ in 1..len {
            text.push(char::from(TAIL[self.below(TAIL.len())]));
        }
        text
    }

    /// A token-shaped secret value, e.g. `sk-live-<12 hex digits>`.
    #[must_use]
    pub fn token(&mut self) -> String {
        let mut token = String::from("sk-live-");
        for _ in 0..12 {
            token.push(char::from(b"0123456789abcdef"[self.below(16)]));
        }
        token
    }

    /// A string mixing ASCII with non-ASCII letters, an emoji and a combining mark.
    #[must_use]
    pub fn unicode_word(&mut self) -> String {
        const PARTS: &[&str] = &["ünïcode", "日本語", "ключ", "é", "e\u{301}", "🛠", "ß", "Ω"];
        self.pick(PARTS).to_string()
    }
}

/// Length of the common byte prefix of `left` and `right`.
#[must_use]
pub fn common_prefix_len(left: &[u8], right: &[u8]) -> usize {
    left.iter().zip(right).take_while(|(a, b)| a == b).count()
}

/// Length of the common byte suffix of `left` and `right`, never counting bytes already part
/// of the common prefix.
#[must_use]
pub fn common_suffix_len(left: &[u8], right: &[u8]) -> usize {
    let prefix = common_prefix_len(left, right);
    let limit = left.len().min(right.len()).saturating_sub(prefix);
    let mut matched = 0;
    while matched < limit && left[left.len() - 1 - matched] == right[right.len() - 1 - matched] {
        matched += 1;
    }
    matched
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequences_are_repeatable_and_bounded() {
        let mut first = Rng::new(42);
        let mut second = Rng::new(42);
        for _ in 0..64 {
            assert_eq!(first.next_u64(), second.next_u64());
        }
        let mut rng = Rng::new(9);
        for _ in 0..256 {
            assert!(rng.below(7) < 7);
            assert!((3..=5).contains(&rng.span(3, 5)));
        }
    }

    #[test]
    fn prefix_and_suffix_do_not_overlap() {
        assert_eq!(common_prefix_len(b"abc", b"abd"), 2);
        assert_eq!(common_suffix_len(b"abc", b"xbc"), 2);
        assert_eq!(common_suffix_len(b"aaaa", b"aaaa"), 0);
        assert_eq!(common_prefix_len(b"", b"x"), 0);
    }
}
