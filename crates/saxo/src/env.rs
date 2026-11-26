pub use crate::live_gate::LIVE_ENABLED;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Env {
    Sim,
    Live,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("live access is disabled")]
pub struct LiveDisabled;

impl Env {
    pub fn base_url(self) -> Result<&'static str, LiveDisabled> {
        match self {
            Self::Sim => Ok("https://gateway.saxobank.com/sim/openapi"),
            Self::Live if LIVE_ENABLED => Ok("https://gateway.saxobank.com/openapi"),
            Self::Live => Err(LiveDisabled),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn live_disabled() {
        assert_eq!(Env::Live.base_url(), Err(LiveDisabled));
    }
}
