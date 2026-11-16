//! `NumericFilter` — the one value type behind every numeric search
//! filter. Parsing lives in `query::parse`, SQL rendering in the catalog;
//! both depend on this type, so it sits in `models`.

/// All numeric search filters (`rating:`, `iso:`, `focal:`, `f:`, `width:`,
/// `height:`, `copies:`, `variants:`, `scattered:`, `faces:N`, `tagcount:`,
/// `duration:`, `bpm:`, `stale:`) use this type for consistent syntax:
/// `x` (exact), `x+` (minimum), `x-y` (range), `x,y` (OR), `x,y+` (combined).
#[derive(Debug, Clone, PartialEq)]
pub enum NumericFilter {
    /// Exactly this value
    Exact(f64),
    /// This value or more
    Min(f64),
    /// Between min and max (inclusive)
    Range(f64, f64),
    /// Any of these exact values
    Values(Vec<f64>),
    /// Any of these exact values OR >= min
    ValuesOrMin { values: Vec<f64>, min: f64 },
}

impl NumericFilter {
    /// Merge another filter (from default_filter) if self is None.
    pub fn or(a: &Option<Self>, b: &Option<Self>) -> Option<Self> {
        a.clone().or_else(|| b.clone())
    }
}
