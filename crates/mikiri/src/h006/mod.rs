//! H006 intraday calculations.

/// SplitMix64 with the preregistered seed.
pub fn splitmix64(n: u64) -> u64 {
    let mut v = (n ^ 20_261_115).wrapping_add(0x9e3779b97f4a7c15);
    v = (v ^ (v >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    v = (v ^ (v >> 27)).wrapping_mul(0x94d049bb133111eb);
    v ^ (v >> 31)
}

#[cfg(test)]
mod tests {
    use super::splitmix64;

    #[test]
    fn splitmix_reference_values() {
        for (n, expected) in [
            (0, 13866863570256181660),
            (1, 8826110057919892546),
            (2, 17342297256639978221),
            (4294967296, 5572763319858028600),
            (4290672337505, 11747482445946130720),
        ] {
            assert_eq!(splitmix64(n), expected);
        }
    }
}

pub mod daily;
pub mod input;
pub mod random;
pub mod reports;
pub mod segments;
