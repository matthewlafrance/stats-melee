use std::collections::HashMap;

pub struct WinProportion {
    pub wins: i32,
    pub total: i32,
}

pub struct WinAnalytics {
    pub opponents: HashMap<String, WinProportion>,
    pub stages: HashMap<i32, WinProportion>,
    pub played_characters: HashMap<i32, WinProportion>,
    pub opp_characters: HashMap<i32, WinProportion>,
}