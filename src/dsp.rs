#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelMode {
    Both,
    Left { mirror: bool },
    Right { mirror: bool },
}

impl ChannelMode {
    pub fn to_code(self) -> u8 {
        match self {
            ChannelMode::Both => 0,
            ChannelMode::Left { mirror: false } => 1,
            ChannelMode::Left { mirror: true } => 2,
            ChannelMode::Right { mirror: false } => 3,
            ChannelMode::Right { mirror: true } => 4,
        }
    }

    pub fn from_code(code: u8) -> ChannelMode {
        match code {
            1 => ChannelMode::Left { mirror: false },
            2 => ChannelMode::Left { mirror: true },
            3 => ChannelMode::Right { mirror: false },
            4 => ChannelMode::Right { mirror: true },
            _ => ChannelMode::Both,
        }
    }

    /// Applies this mode's gain matrix to a single (left, right) sample pair.
    pub fn apply(self, in_l: f32, in_r: f32) -> (f32, f32) {
        match self {
            ChannelMode::Both => (in_l, in_r),
            ChannelMode::Left { mirror: false } => (in_l, 0.0),
            ChannelMode::Left { mirror: true } => (in_l, in_l),
            ChannelMode::Right { mirror: false } => (0.0, in_r),
            ChannelMode::Right { mirror: true } => (in_r, in_r),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_passes_through() {
        assert_eq!(ChannelMode::Both.apply(0.3, 0.7), (0.3, 0.7));
    }

    #[test]
    fn left_no_mirror_mutes_right() {
        assert_eq!(ChannelMode::Left { mirror: false }.apply(0.3, 0.7), (0.3, 0.0));
    }

    #[test]
    fn left_mirror_copies_left_into_right() {
        assert_eq!(ChannelMode::Left { mirror: true }.apply(0.3, 0.7), (0.3, 0.3));
    }

    #[test]
    fn right_no_mirror_mutes_left() {
        assert_eq!(ChannelMode::Right { mirror: false }.apply(0.3, 0.7), (0.0, 0.7));
    }

    #[test]
    fn right_mirror_copies_right_into_left() {
        assert_eq!(ChannelMode::Right { mirror: true }.apply(0.3, 0.7), (0.7, 0.7));
    }

    #[test]
    fn code_roundtrip() {
        for mode in [
            ChannelMode::Both,
            ChannelMode::Left { mirror: false },
            ChannelMode::Left { mirror: true },
            ChannelMode::Right { mirror: false },
            ChannelMode::Right { mirror: true },
        ] {
            assert_eq!(ChannelMode::from_code(mode.to_code()), mode);
        }
    }
}
