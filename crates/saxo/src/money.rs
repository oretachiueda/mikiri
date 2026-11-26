#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Milli(pub i64);

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("price is not an exact milli-unit")]
pub struct InvalidPrice;

impl TryFrom<f64> for Milli {
    type Error = InvalidPrice;
    fn try_from(value: f64) -> Result<Self, Self::Error> {
        let scaled = value * 1000.0;
        let rounded = scaled.round();
        if !value.is_finite()
            || rounded < i64::MIN as f64
            || rounded >= i64::MAX as f64
            || (value - rounded / 1000.0).abs() > 1e-9
        {
            return Err(InvalidPrice);
        }
        Ok(Self(rounded as i64))
    }
}
impl From<Milli> for f64 {
    fn from(value: Milli) -> Self {
        value.0 as f64 / 1000.0
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn roundtrip() {
        assert_eq!(
            Milli::try_from(f64::from(Milli(146_819))),
            Ok(Milli(146_819))
        );
        assert!(Milli::try_from(146.8191).is_err());
    }
}
