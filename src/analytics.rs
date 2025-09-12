use std::collections::HashMap;
use std::fmt;
use crate::gamedata::{STAGES, CHARACTERS};
use crate::NUM_STAGES;
use crate::NUM_CHARACTERS;

#[derive(Debug, Copy, Clone)]
pub struct WinProportion {
    pub wins: i32,
    pub total: i32,
    pub proportion: f32,
}

impl WinProportion {
    pub fn new_winproportion() -> WinProportion {
        WinProportion {wins: 0, total: 0, proportion: 0.0}
    }

    pub fn update_proportion(&mut self) {
        if self.total != 0 {
            self.proportion = self.wins as f32 / self.total as f32;
        } else {
            self.proportion = 0.0;
        }
    }
}

impl fmt::Display for WinProportion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}% winrate, {} wins, {} total", (self.proportion * 10000.0).round() /100.0, self.wins, self.total)
    }
}

#[derive(Debug)]
pub struct WinAnalytics {
    pub opponents: HashMap<String, WinProportion>,
    pub stages: [WinProportion; NUM_STAGES],
    pub played_characters: [WinProportion; NUM_CHARACTERS],
    pub opp_characters: [WinProportion; NUM_CHARACTERS],
}

impl fmt::Display for WinAnalytics {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {

        writeln!(f, "Opponent stats (top 50 by total):")?;
        let mut opponents_sorted: Vec<_> = self
            .opponents
            .iter()
            .filter(|(_, wp)| wp.total > 0)
            .collect();
        opponents_sorted.sort_by(|a, b| b.1.total.cmp(&a.1.total));
        for (opponent, wp) in opponents_sorted.into_iter().take(50) {
            writeln!(f, "  {} -- {}", opponent, wp)?;
        }

        writeln!(f, "\nStage stats:")?;
        let mut stages_sorted: Vec<_> = self
            .stages
            .iter()
            .enumerate()
            .filter(|(_, wp)| wp.total > 0)
            .collect();
        stages_sorted.sort_by(|a, b| b.1.total.cmp(&a.1.total));
        for (i, wp) in stages_sorted {
            writeln!(f, "  {} -- {}", STAGES[i], wp)?;
        }

        writeln!(f, "\nPlayed character stats:")?;
        let mut played_sorted: Vec<_> = self
            .played_characters
            .iter()
            .enumerate()
            .filter(|(_, wp)| wp.total > 0)
            .collect();
        played_sorted.sort_by(|a, b| b.1.total.cmp(&a.1.total));
        for (i, wp) in played_sorted {
            writeln!(f, "  {} -- {}", CHARACTERS[i], wp)?;
        }

        writeln!(f, "\nOpponent character stats:")?;
        let mut opp_sorted: Vec<_> = self
            .opp_characters
            .iter()
            .enumerate()
            .filter(|(_, wp)| wp.total > 0)
            .collect();
        opp_sorted.sort_by(|a, b| b.1.total.cmp(&a.1.total));
        for (i, wp) in opp_sorted {
            writeln!(f, "  {} -- {}", CHARACTERS[i], wp)?;
        }

        Ok(())
    }
}