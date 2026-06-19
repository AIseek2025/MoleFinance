//! Core enums shared across the clearing engine.

/// Trade direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Direction {
    /// Long: profits when price rises.
    Long,
    /// Short: profits when price falls.
    Short,
}

impl Direction {
    /// Numeric direction sign: `+1` for long, `-1` for short.
    pub fn sign(self) -> i8 {
        match self {
            Direction::Long => 1,
            Direction::Short => -1,
        }
    }

    /// Counterparty direction.
    pub fn opposite(self) -> Direction {
        match self {
            Direction::Long => Direction::Short,
            Direction::Short => Direction::Long,
        }
    }
}
