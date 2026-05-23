//! Shared value types for the ML integration boundary.
//!
//! Kept dependency-free so consuming crates can construct these
//! without dragging in anything ML-specific.

/// A confidence score in the closed interval [0.0, 1.0].
///
/// Constructed via [`Confidence::new`], which clamps out-of-range
/// and NaN inputs. We deliberately do *not* implement `Eq`; use
/// `PartialOrd` / `partial_cmp` for comparisons.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct Confidence(f64);

impl Confidence {
    pub fn new(v: f64) -> Self {
        if v.is_nan() {
            Confidence(0.0)
        } else if v < 0.0 {
            Confidence(0.0)
        } else if v > 1.0 {
            Confidence(1.0)
        } else {
            Confidence(v)
        }
    }

    pub fn zero() -> Self {
        Confidence(0.0)
    }

    pub fn one() -> Self {
        Confidence(1.0)
    }

    pub fn value(self) -> f64 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confidence_clamps_to_unit_interval() {
        assert_eq!(Confidence::new(0.5).value(), 0.5);
        assert_eq!(Confidence::new(-0.1).value(), 0.0);
        assert_eq!(Confidence::new(1.7).value(), 1.0);
        assert_eq!(Confidence::new(f64::NAN).value(), 0.0);
    }

    #[test]
    fn confidence_zero_and_one_helpers() {
        assert_eq!(Confidence::zero().value(), 0.0);
        assert_eq!(Confidence::one().value(), 1.0);
    }

    #[test]
    fn confidence_is_ordered() {
        assert!(Confidence::new(0.1) < Confidence::new(0.9));
    }
}
