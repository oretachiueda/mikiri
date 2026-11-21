use super::{
    Result,
    market::{Account, DAYS_PER_YEAR},
    mean, sample_variance,
};
use std::{collections::BTreeSet, f64::consts};

pub(super) const GRID_SIZE: usize = 24;
#[allow(
    clippy::approx_constant,
    reason = "S002 preregisters ten decimal places"
)]
const EULER: f64 = 0.577_215_664_9;
const D_COUNT: usize = 1_000;

pub(super) fn sr(values: &[f64]) -> Result<f64> {
    let sd = sample_variance(values).sqrt();
    let mean = mean(values);
    let sr = mean / sd;
    if !sr.is_finite() || sd <= 0.0 {
        return Err("invalid daily Sharpe (zero or non-finite variance)".into());
    }
    Ok(sr)
}

pub(super) fn after(account: &Account) -> Vec<f64> {
    account.days.iter().map(|day| day.after).collect()
}

pub(super) fn annual(sr: f64) -> f64 {
    sr * DAYS_PER_YEAR.sqrt()
}

pub(super) fn max_drawdown(values: &[f64]) -> f64 {
    let mut wealth: f64 = 1.0;
    let mut peak: f64 = 1.0;
    let mut max_dd: f64 = 0.0;
    for &r in values {
        wealth *= 1.0 + r;
        peak = peak.max(wealth);
        max_dd = max_dd.max(1.0 - wealth / peak);
    }
    max_dd
}

fn cdf(z: f64) -> f64 {
    0.5 * libm::erfc(-z / consts::SQRT_2)
}

fn quantile(p: f64) -> f64 {
    let mut lo = -10.0;
    let mut hi = 10.0;
    for _ in 0..56 {
        let mid = (lo + hi) / 2.0;
        if cdf(mid) < p {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    (hi + lo) / 2.0
}

pub(super) fn expected_max_sr(variance: f64) -> f64 {
    let k = GRID_SIZE as f64;
    variance.sqrt()
        * ((1.0 - EULER) * quantile(1.0 - 1.0 / k) + EULER * quantile(1.0 - 1.0 / (k * consts::E)))
}

pub(super) fn psr(values: &[f64], sr: f64, benchmark: f64) -> Result<f64> {
    let n = values.len() as f64;
    let mean = mean(values);
    let sd = (sample_variance(values) * (n - 1.0) / n).sqrt();
    let (third, fourth) = values.iter().fold((0.0, 0.0), |(third, fourth), value| {
        let z = (value - mean) / sd;
        (third + z.powi(3), fourth + z.powi(4))
    });
    let skew = third / n;
    let kurtosis = fourth / n;
    let variance = 1.0 - skew * sr + (kurtosis - 1.0) * sr.powi(2) / 4.0;
    if !variance.is_finite() || variance <= 0.0 {
        return Err("invalid PSR variance".into());
    }
    Ok(cdf((sr - benchmark) * ((n - 1.0) / variance).sqrt()))
}

pub(super) fn offsets(t: usize) -> Result<Vec<usize>> {
    if t < 503 + D_COUNT {
        return Err("D needs at least 1,503 dev dates to draw 1,000 distinct offsets".into());
    }
    let mut drawn = BTreeSet::new();
    let mut offsets = Vec::with_capacity(D_COUNT);
    let mut state = 2_026_112_101_u64;
    while offsets.len() < D_COUNT {
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let z = (state ^ (state >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        let z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        let x = z ^ (z >> 31);
        let offset = 252 + (x % (t - 503) as u64) as usize;
        if drawn.insert(offset) {
            offsets.push(offset);
        }
    }
    Ok(offsets)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splitmix_offsets_are_distinct_bounded_and_match_reference() {
        let offsets = offsets(7_258).unwrap();
        assert_eq!(
            offsets[..10],
            [1501, 3107, 5893, 5182, 6974, 6493, 1900, 5680, 1960, 931]
        );
        assert_eq!(
            offsets.iter().copied().collect::<BTreeSet<_>>().len(),
            D_COUNT
        );
        assert!(offsets.iter().all(|s| (252..=7_006).contains(s)));
        assert!(super::offsets(1_502).is_err());
    }

    #[test]
    fn sample_sd_and_non_excess_kurtosis_drive_psr_and_dsr() {
        let r = [0.00, 0.01, 0.02, 0.03];
        let daily = sr(&r).unwrap();
        assert!((daily - 0.015 / (0.0005_f64 / 3.0).sqrt()).abs() < 1e-14);
        let n = 4.0;
        let kurtosis = 1.64;
        let variance = 1.0 + (kurtosis - 1.0) * daily * daily / 4.0;
        let expected = cdf(daily * ((n - 1.0) / variance).sqrt());
        assert!((psr(&r, daily, 0.0).unwrap() - expected).abs() < 1e-14);
        let benchmark = expected_max_sr(0.0025);
        assert!((benchmark - 0.05 * 1.980).abs() < 5e-5);
        assert_eq!(psr(&r, daily, daily).unwrap(), 0.5);
    }
}
