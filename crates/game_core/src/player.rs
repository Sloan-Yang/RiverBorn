//! 玩家状态。

use crate::cards::Adventurer;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PlayerId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayerStatus {
    Active, // 还在本局，可以行动
    Folded, // 已弃牌（撤退）
    AllIn,  // 已全下，后续轮次不再行动
}

#[derive(Debug, Clone)]
pub struct Player {
    pub id: PlayerId,
    pub name: String,
    pub is_ai: bool,
    pub chips: u32,           // 剩余筹码
    pub hole: [Adventurer; 2], // 2 张底牌冒险者（隐藏）
    pub status: PlayerStatus,
    /// 本下注阶段已投入的筹码（每进入新阶段清零）。
    pub committed: u32,
}

impl Player {
    pub fn new(id: u32, name: impl Into<String>, is_ai: bool, chips: u32, hole: [Adventurer; 2]) -> Self {
        Self {
            id: PlayerId(id),
            name: name.into(),
            is_ai,
            chips,
            hole,
            status: PlayerStatus::Active,
            committed: 0,
        }
    }

    pub fn is_in_hand(&self) -> bool {
        self.status != PlayerStatus::Folded
    }

    /// 还能主动行动（没弃牌、没全下、还有筹码）。
    pub fn can_act(&self) -> bool {
        self.status == PlayerStatus::Active && self.chips > 0
    }
}
