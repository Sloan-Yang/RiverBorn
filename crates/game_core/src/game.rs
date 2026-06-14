//! 顶层游戏状态机：下注轮 + 阶段推进 + 战斗结算。
//!
//! 对应 README 的流程：Pre-flop → Flop → Turn → River → 摊牌。
//! 这里实现的是「单局（one hand）」的完整流程，逻辑全部确定性、
//! 不依赖引擎，可用 `cargo test -p game_core` 验证。

use crate::cards::*;
use crate::player::*;
use crate::rng::Rng;

/// 下注阶段。每个阶段翻开对应的公共牌 / 地牢节点。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    PreFlop, // 只看底牌下注
    Flop,    // +3 公共牌, +1 地牢节点
    Turn,    // +1 公共牌, +1 地牢节点
    River,   // +1 公共牌, +Boss
    Showdown, // 摊牌：构筑队伍跑地牢
    Done,    // 本局结束
}

/// 玩家在某个下注轮可以做的动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Fold,
    Check,
    Call,
    /// 把本阶段的「下注线」抬高到 `to`（总额，不是增量）。
    Raise { to: u32 },
    AllIn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionError {
    NotPlayersTurn,
    CannotCheck,   // 当前有人下注，必须 call/raise/fold
    RaiseTooSmall, // raise 必须高于当前下注线
    NotEnoughChips,
    HandOver,
}

pub struct Game {
    pub players: Vec<Player>,
    pub phase: Phase,
    pub pot: u32,
    /// 本阶段需要跟到的下注线。
    pub current_bet: u32,
    /// 当前轮到行动的玩家索引。
    pub to_act: usize,
    /// 自上次加注以来，该玩家是否已经行动过。
    acted: Vec<bool>,

    pub community: Vec<CommunityCard>, // 已翻开的公共牌（最多 5）
    pub dungeon: Vec<Monster>,         // 已翻开的地牢节点（最多 3）

    community_deck: Vec<CommunityCard>,
    dungeon_deck: Vec<Monster>,
}

impl Game {
    /// 用给定 seed 开新的一局。`ante` 为每人前注。
    pub fn new(mut players: Vec<Player>, ante: u32, seed: u64) -> Self {
        let mut rng = Rng::new(seed);

        // 收前注，进底池。
        let mut pot = 0;
        for p in &mut players {
            let a = ante.min(p.chips);
            p.chips -= a;
            p.committed = a;
            pot += a;
        }

        let n = players.len();
        let mut community_deck = sample_community_deck();
        let mut dungeon_deck = sample_dungeon_deck();
        rng.shuffle(&mut community_deck);
        rng.shuffle(&mut dungeon_deck);

        let mut game = Self {
            players,
            phase: Phase::PreFlop,
            pot,
            current_bet: ante, // preflop 大家都已交 ante，下注线 = ante
            to_act: 0,
            acted: vec![false; n],
            community: Vec::new(),
            dungeon: Vec::new(),
            community_deck,
            dungeon_deck,
        };
        game.to_act = game.first_to_act();
        game
    }

    // ---- 查询 ----

    fn first_to_act(&self) -> usize {
        (0..self.players.len())
            .find(|&i| self.players[i].can_act())
            .unwrap_or(0)
    }

    /// 该玩家本轮是否还需要行动。
    fn needs_action(&self, i: usize) -> bool {
        let p = &self.players[i];
        p.can_act() && (!self.acted[i] || p.committed < self.current_bet)
    }

    /// 仍未弃牌的玩家数。
    pub fn players_in_hand(&self) -> usize {
        self.players.iter().filter(|p| p.is_in_hand()).count()
    }

    /// 当前是否轮到该玩家行动（供 UI / AI 查询）。
    pub fn active_player(&self) -> Option<&Player> {
        self.players.get(self.to_act)
    }

    // ---- 行动 ----

    /// 提交当前行动玩家的动作，并在下注轮结束时自动推进阶段。
    pub fn apply(&mut self, action: Action) -> Result<(), ActionError> {
        if self.phase == Phase::Showdown || self.phase == Phase::Done {
            return Err(ActionError::HandOver);
        }
        if !self.players[self.to_act].can_act() {
            return Err(ActionError::NotPlayersTurn);
        }

        let i = self.to_act;
        match action {
            Action::Fold => {
                self.players[i].status = PlayerStatus::Folded;
            }
            Action::Check => {
                if self.players[i].committed < self.current_bet {
                    return Err(ActionError::CannotCheck);
                }
            }
            Action::Call => {
                let need = self.current_bet.saturating_sub(self.players[i].committed);
                let pay = need.min(self.players[i].chips);
                self.commit(i, pay);
                if self.players[i].chips == 0 {
                    self.players[i].status = PlayerStatus::AllIn;
                }
            }
            Action::Raise { to } => {
                if to <= self.current_bet {
                    return Err(ActionError::RaiseTooSmall);
                }
                let need = to - self.players[i].committed;
                if need > self.players[i].chips {
                    return Err(ActionError::NotEnoughChips);
                }
                self.commit(i, need);
                self.current_bet = to;
                self.reset_acted_after_raise(i);
            }
            Action::AllIn => {
                let all = self.players[i].chips;
                self.commit(i, all);
                self.players[i].status = PlayerStatus::AllIn;
                if self.players[i].committed > self.current_bet {
                    self.current_bet = self.players[i].committed;
                    self.reset_acted_after_raise(i);
                }
            }
        }

        self.acted[i] = true;
        self.advance_turn();
        Ok(())
    }

    fn commit(&mut self, i: usize, amount: u32) {
        let amount = amount.min(self.players[i].chips);
        self.players[i].chips -= amount;
        self.players[i].committed += amount;
        self.pot += amount;
    }

    /// 有人加注后，其它人需要重新响应。
    fn reset_acted_after_raise(&mut self, raiser: usize) {
        for a in &mut self.acted {
            *a = false;
        }
        self.acted[raiser] = true;
    }

    fn advance_turn(&mut self) {
        // 只剩一人未弃牌 → 直接进结算（bluff 规则在结算处理）。
        if self.players_in_hand() <= 1 {
            self.goto_showdown();
            return;
        }
        // 找下一个需要行动的玩家。
        let n = self.players.len();
        for step in 1..=n {
            let idx = (self.to_act + step) % n;
            if self.needs_action(idx) {
                self.to_act = idx;
                return;
            }
        }
        // 没人需要行动 → 本下注轮结束。
        self.next_phase();
    }

    // ---- 阶段推进 ----

    fn start_betting_round(&mut self) {
        self.current_bet = 0;
        for p in &mut self.players {
            p.committed = 0;
        }
        for a in &mut self.acted {
            *a = false;
        }
        self.to_act = self.first_to_act();
        // 若已无人能主动行动（都 all-in / 只剩一人），直接快进。
        if !self.players.iter().any(|p| p.can_act()) || self.players_in_hand() <= 1 {
            self.next_phase();
        }
    }

    fn next_phase(&mut self) {
        match self.phase {
            Phase::PreFlop => {
                self.phase = Phase::Flop;
                self.deal_community(3);
                self.deal_dungeon(1);
                self.start_betting_round();
            }
            Phase::Flop => {
                self.phase = Phase::Turn;
                self.deal_community(1);
                self.deal_dungeon(1);
                self.start_betting_round();
            }
            Phase::Turn => {
                self.phase = Phase::River;
                self.deal_community(1);
                self.deal_dungeon(1); // Boss
                self.start_betting_round();
            }
            Phase::River => self.goto_showdown(),
            Phase::Showdown | Phase::Done => {}
        }
    }

    fn goto_showdown(&mut self) {
        self.phase = Phase::Showdown;
    }

    fn deal_community(&mut self, n: usize) {
        for _ in 0..n {
            if let Some(c) = self.community_deck.pop() {
                self.community.push(c);
            }
        }
    }

    fn deal_dungeon(&mut self, n: usize) {
        for _ in 0..n {
            if let Some(m) = self.dungeon_deck.pop() {
                self.dungeon.push(m);
            }
        }
    }

    // ---- 摊牌 / 战斗结算 ----

    /// 对所有未弃牌的玩家跑当前已揭示的地牢，返回结算结果。
    ///
    /// 简化的占位平衡：队伍战力/生命由「2 底牌 + 公共池里的角色与装备」汇总，
    /// 逐个节点结算。真正的数值/克制平衡后续再调，这里只保证流程闭环。
    pub fn settle(&self) -> Settlement {
        let team_buff = community_team_buff(&self.community);

        let mut results = Vec::new();
        for p in &self.players {
            if !p.is_in_hand() {
                continue;
            }
            let team = build_team(&p.hole, team_buff);
            let outcome = run_dungeon(team, &self.dungeon);
            results.push(PlayerResult {
                id: p.id,
                cleared: outcome.is_some(),
                remaining_health: outcome.unwrap_or(0),
            });
        }

        // 通关者中剩余生命最高者拿主池；无人通关则主池滚入下一局 jackpot。
        let winner = results
            .iter()
            .filter(|r| r.cleared)
            .max_by_key(|r| r.remaining_health)
            .map(|r| r.id);

        Settlement {
            pot: self.pot,
            winner,
            results,
        }
    }
}

/// 队伍的汇总属性。
#[derive(Debug, Clone, Copy)]
struct Team {
    power: u32,
    health: u32,
}

fn build_team(hole: &[Adventurer; 2], buff: (u32, u32)) -> Team {
    let mut power = buff.0;
    let mut health = buff.1;
    for a in hole {
        power += a.power;
        health += a.health;
    }
    Team { power, health }
}

/// 公共池里的角色与装备给队伍带来的加成。
fn community_team_buff(community: &[CommunityCard]) -> (u32, u32) {
    let mut power = 0;
    let mut health = 0;
    for c in community {
        match c {
            CommunityCard::Unit(a) => {
                power += a.power;
                health += a.health;
            }
            CommunityCard::Gear { bonus_power, bonus_health } => {
                power += bonus_power;
                health += bonus_health;
            }
        }
    }
    (power, health)
}

/// 跑地牢：逐节点结算。返回通关后剩余生命，团灭返回 None。
fn run_dungeon(mut team: Team, dungeon: &[Monster]) -> Option<u32> {
    for node in dungeon {
        // 毒沼泽：克制低生命队伍，额外掉血。
        if node.kind == MonsterKind::PoisonSwamp && team.health < node.health {
            team.health = team.health.saturating_sub(node.threat / 2);
        }
        // 宝箱无威胁，跳过。
        if node.kind == MonsterKind::Treasure {
            continue;
        }
        // 战力压不过威胁 → 打不动 → 团灭。
        if team.power < node.threat {
            return None;
        }
        // 承伤 = 节点威胁；扛不住则团灭。
        if team.health <= node.threat {
            return None;
        }
        team.health -= node.threat;
    }
    Some(team.health)
}

#[derive(Debug, Clone)]
pub struct PlayerResult {
    pub id: PlayerId,
    pub cleared: bool,
    pub remaining_health: u32,
}

#[derive(Debug, Clone)]
pub struct Settlement {
    pub pot: u32,
    /// 主池赢家；None 表示无人通关，主池滚入下一局 jackpot。
    pub winner: Option<PlayerId>,
    pub results: Vec<PlayerResult>,
}

// ---- 占位牌库（之后替换为真正的卡牌设计）----

fn sample_community_deck() -> Vec<CommunityCard> {
    use Class::*;
    vec![
        CommunityCard::Unit(Adventurer::new(Warrior, 3, 8)),
        CommunityCard::Unit(Adventurer::new(Cleric, 2, 5)),
        CommunityCard::Unit(Adventurer::new(Mage, 6, 3)),
        CommunityCard::Unit(Adventurer::new(Rogue, 5, 4)),
        CommunityCard::Unit(Adventurer::new(Ranger, 4, 5)),
        CommunityCard::Gear { bonus_power: 3, bonus_health: 0 },
        CommunityCard::Gear { bonus_power: 0, bonus_health: 6 },
        CommunityCard::Unit(Adventurer::new(Warrior, 2, 10)),
        CommunityCard::Unit(Adventurer::new(Mage, 7, 2)),
        CommunityCard::Gear { bonus_power: 2, bonus_health: 4 },
    ]
}

fn sample_dungeon_deck() -> Vec<Monster> {
    vec![
        Monster::new(MonsterKind::Goblin, 5, 4),
        Monster::new(MonsterKind::Elite, 9, 8),
        Monster::new(MonsterKind::PoisonSwamp, 6, 12),
        Monster::new(MonsterKind::Treasure, 0, 0),
        Monster::new(MonsterKind::Boss, 14, 16),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn demo_players(n: u32, chips: u32) -> Vec<Player> {
        use Class::*;
        (0..n)
            .map(|i| {
                Player::new(
                    i,
                    format!("P{i}"),
                    i != 0, // P0 是人类，其余 AI
                    chips,
                    [Adventurer::new(Warrior, 4, 9), Adventurer::new(Mage, 6, 3)],
                )
            })
            .collect()
    }

    #[test]
    fn ante_collected_into_pot() {
        let g = Game::new(demo_players(3, 100), 5, 42);
        assert_eq!(g.pot, 15);
        assert!(g.players.iter().all(|p| p.chips == 95));
        assert_eq!(g.phase, Phase::PreFlop);
    }

    #[test]
    fn checking_around_advances_phase_and_deals() {
        // 3 人 ante 后下注线 = 5，且大家都已 committed 5，可以一路 check。
        let mut g = Game::new(demo_players(3, 100), 5, 1);
        for _ in 0..3 {
            g.apply(Action::Check).unwrap();
        }
        assert_eq!(g.phase, Phase::Flop);
        assert_eq!(g.community.len(), 3);
        assert_eq!(g.dungeon.len(), 1);
    }

    #[test]
    fn raise_forces_others_to_respond() {
        let mut g = Game::new(demo_players(3, 100), 5, 7);
        // P0 加注到 20。
        g.apply(Action::Raise { to: 20 }).unwrap();
        // 还没结束，轮到 P1。
        assert_eq!(g.phase, Phase::PreFlop);
        g.apply(Action::Call).unwrap(); // P1 跟
        g.apply(Action::Fold).unwrap(); // P2 弃
        // P0、P1 跟到 20，轮结束 → Flop。
        assert_eq!(g.phase, Phase::Flop);
        assert_eq!(g.players_in_hand(), 2);
        // 底池：3*5(ante) + (20-5)*2 = 15 + 30 = 45
        assert_eq!(g.pot, 45);
    }

    #[test]
    fn everyone_folds_to_one_goes_to_showdown() {
        let mut g = Game::new(demo_players(3, 100), 5, 3);
        g.apply(Action::Raise { to: 30 }).unwrap();
        g.apply(Action::Fold).unwrap();
        g.apply(Action::Fold).unwrap();
        // 只剩 P0，但 README 的 bluff 规则：仍要跑地牢才算赢。
        assert_eq!(g.phase, Phase::Showdown);
        assert_eq!(g.players_in_hand(), 1);
    }

    #[test]
    fn full_hand_reaches_showdown_and_settles() {
        let mut g = Game::new(demo_players(2, 200), 10, 99);
        // 一路 check 到 River 之后摊牌。
        let mut guard = 0;
        while g.phase != Phase::Showdown && guard < 50 {
            // 当前玩家能 check 就 check，否则 call。
            let p = g.active_player().unwrap();
            let act = if p.committed >= g.current_bet {
                Action::Check
            } else {
                Action::Call
            };
            g.apply(act).unwrap();
            guard += 1;
        }
        assert_eq!(g.phase, Phase::Showdown);
        assert_eq!(g.community.len(), 5);
        assert_eq!(g.dungeon.len(), 3);

        let s = g.settle();
        // 结算要么有人通关拿池，要么无人通关（winner = None）。
        assert_eq!(s.pot, g.pot);
        assert_eq!(s.results.len(), 2);
    }
}
