//! 顶层游戏状态机：下注轮 + 阶段推进 + 战斗结算。
//!
//! 对应 README 的流程：Pre-flop → Flop → Turn → River → 摊牌。
//! 这里实现的是「单局（one hand）」的完整流程，逻辑全部确定性、
//! 不依赖引擎，可用 `cargo test -p game_core` 验证。

use crate::cards::*;
use crate::player::*;
use crate::rng::Rng;

/// 触发 Boss 狂暴的「单押 all-in」金额下限：押上去 ≥ 这个数才算大额梭哈。
pub const BIG_ALLIN: u32 = 500;

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

    /// 本手的牌桌场景（给闯关队伍挂 buff）。由 Match 在发牌时设定。
    pub scene: Scene,

    /// 惊动值：下注越凶，地牢精英/Boss 越危险（见 `combat`）。
    pub aggro: f32,
    /// Boss 是否狂暴（有人 All-in 触发，威胁翻倍）。
    pub boss_berserk: bool,

    pub community: Vec<CommunityCard>, // 已翻开的公共牌（最多 5）
    pub dungeon: Vec<Monster>,         // 已翻开的地牢节点（最多 3）

    community_deck: Vec<CommunityCard>,
    // 地牢分三档，按阶段发：Flop 小怪、Turn 精英、River Boss。
    small_deck: Vec<Monster>,
    elite_deck: Vec<Monster>,
    boss_deck: Vec<Monster>,
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
        let (mut small_deck, mut elite_deck, mut boss_deck) = sample_dungeon_decks();
        rng.shuffle(&mut community_deck);
        rng.shuffle(&mut small_deck);
        rng.shuffle(&mut elite_deck);
        rng.shuffle(&mut boss_deck);

        let mut game = Self {
            players,
            phase: Phase::PreFlop,
            pot,
            current_bet: ante, // preflop 大家都已交 ante，下注线 = ante
            to_act: 0,
            acted: vec![false; n],
            scene: Scene::StoneArena, // 中性默认，Match 发牌时会 set_scene 覆盖
            aggro: 0.0,
            boss_berserk: false,
            community: Vec::new(),
            dungeon: Vec::new(),
            community_deck,
            small_deck,
            elite_deck,
            boss_deck,
        };
        game.to_act = game.first_to_act();
        game
    }

    /// 设定本手场景（Match 在发牌后调用）。
    pub fn set_scene(&mut self, scene: Scene) {
        self.scene = scene;
    }

    /// 把彩池滚存等额外筹码并入底池（无人通关时的 jackpot 滚到下一手）。
    pub fn add_to_pot(&mut self, amount: u32) {
        self.pot += amount;
    }

    /// 标记本手已结束（结算完成后由 Match 调用，防止重复结算 / 继续行动）。
    pub fn finish(&mut self) {
        self.phase = Phase::Done;
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
                self.aggro += 0.5; // 跟注：轻微惊动
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
                self.aggro += 1.0; // 加注：惊动 +1
            }
            Action::AllIn => {
                let shove = self.players[i].chips; // 这次 all-in 押上的金额
                self.commit(i, shove);
                self.players[i].status = PlayerStatus::AllIn;
                let raised = self.players[i].committed > self.current_bet;
                if raised {
                    self.current_bet = self.players[i].committed;
                    self.reset_acted_after_raise(i);
                }
                // 惊动：只有**单押 ≥ 500 的大额 all-in**才让 Boss 狂暴并大幅升惊动；
                // 小额 all-in（比如只剩 20 块被迫梭）只当普通加注/跟注。
                if shove >= BIG_ALLIN {
                    self.aggro += 3.0;
                    self.boss_berserk = true;
                } else if raised {
                    self.aggro += 1.0;
                } else {
                    self.aggro += 0.5;
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
        // 火山：每推进一个下注轮，Boss 惊动 +1（拖得越久越热）。
        if self.scene == Scene::Volcano && matches!(self.phase, Phase::PreFlop | Phase::Flop | Phase::Turn) {
            self.aggro += 1.0;
        }
        match self.phase {
            Phase::PreFlop => {
                self.phase = Phase::Flop;
                self.deal_community(3);
                if let Some(m) = self.small_deck.pop() {
                    self.dungeon.push(m); // 小怪
                }
                self.start_betting_round();
            }
            Phase::Flop => {
                self.phase = Phase::Turn;
                self.deal_community(1);
                if let Some(m) = self.elite_deck.pop() {
                    self.dungeon.push(m); // 精英 / 环境
                }
                self.start_betting_round();
            }
            Phase::Turn => {
                self.phase = Phase::River;
                self.deal_community(1);
                if let Some(m) = self.boss_deck.pop() {
                    self.dungeon.push(m); // Boss
                }
                self.start_betting_round();
            }
            Phase::River => self.goto_showdown(),
            Phase::Showdown | Phase::Done => {}
        }
    }

    fn goto_showdown(&mut self) {
        // 全员弃牌到只剩一人可能在 River 之前触发摊牌。为了让结算用**完整牌面**、
        // 并让独狼诈唬也必须跑「小怪 + 精英 + Boss」整条地牢（bluff 规则），
        // 这里把牌面「跑满」到 River：公共牌补到 5 张，地牢补齐三档各一个。
        self.deal_community(5usize.saturating_sub(self.community.len()));
        if self.dungeon.is_empty() {
            if let Some(m) = self.small_deck.pop() {
                self.dungeon.push(m);
            }
        }
        if self.dungeon.len() < 2 {
            if let Some(m) = self.elite_deck.pop() {
                self.dungeon.push(m);
            }
        }
        if self.dungeon.len() < 3 {
            if let Some(m) = self.boss_deck.pop() {
                self.dungeon.push(m);
            }
        }
        self.phase = Phase::Showdown;
    }

    fn deal_community(&mut self, n: usize) {
        for _ in 0..n {
            if let Some(c) = self.community_deck.pop() {
                self.community.push(c);
            }
        }
    }


    // ---- 摊牌 / 战斗结算 ----

    /// 对所有未弃牌的玩家跑地牢并结算。每个玩家默认用 `simulate_clear`
    /// 自动选最优 3 张公共牌（AI 走这条）。
    pub fn settle(&self) -> Settlement {
        self.settle_inner(None)
    }

    /// 结算，但指定 `who` 这位玩家用**手动选择**的 3 张公共牌（人类自己点选的）。
    /// 其余玩家仍自动选最优。
    pub fn settle_with_selection(&self, who: PlayerId, picks: [usize; 3]) -> Settlement {
        self.settle_inner(Some((who, picks)))
    }

    fn settle_inner(&self, manual: Option<(PlayerId, [usize; 3])>) -> Settlement {
        let mut results = Vec::new();
        for p in &self.players {
            if !p.is_in_hand() {
                continue;
            }
            // 指定玩家用手选的 3 张，其余自动选最优。
            let r = match manual {
                Some((id, idx)) if id == p.id => self.clear_with_selection(&p.hole, &idx),
                _ => self.simulate_clear(&p.hole),
            };
            results.push(PlayerResult {
                id: p.id,
                cleared: r.cleared,
                remaining_health: r.remaining_health,
                remaining_power: r.remaining_power,
                advantage: r.advantage(),
                team_power: r.team_power,
                team_health: r.team_health,
            });
        }

        // 通关者中**战后优势值**最高者拿主池；无人通关则主池滚入下一局 jackpot。
        let winner = results
            .iter()
            .filter(|r| r.cleared)
            .max_by_key(|r| r.advantage)
            .map(|r| r.id);

        Settlement {
            pot: self.pot,
            winner,
            results,
        }
    }

    /// 用**指定**的公共牌下标（玩家手选的 3 张）组队跑地牢，不做枚举择优。
    /// 越界下标自动忽略；通常传 3 个合法下标。
    pub fn clear_with_selection(&self, hole: &[Adventurer; 2], indices: &[usize]) -> crate::combat::Report {
        let picks: Vec<CommunityCard> = indices.iter().filter_map(|&i| self.community.get(i).copied()).collect();
        crate::combat::resolve(hole, &picks, self.scene, &self.dungeon, self.aggro, self.boss_berserk)
    }

    /// 队伍构成规则：**2 底牌（固定）+ 已翻公共牌里任选 3 张 + 场景效果 = 5 张**。
    /// 在所有「选 3 张」的组合里挑出**战后优势值最高**的那种（德扑「7 选 5 取最优」）。
    /// 实际组队/战斗（职业搭配、绑定装备、消耗品、惊动、场景）都在 [`crate::combat`]。
    /// 返回最优组合的结果（None = 没有任何组合能通关）。settle 与 AI 估牌都走这里。
    ///
    /// 已翻公共牌 ≤ 3 张时只能全用（PreFlop 估牌或补满后的牌面）；
    /// 4/5 张时枚举 C(n,3) 个组合（最多 10 个，开销极小）。
    pub fn simulate_clear(&self, hole: &[Adventurer; 2]) -> crate::combat::Report {
        let resolve = |picks: &[CommunityCard]| {
            crate::combat::resolve(hole, picks, self.scene, &self.dungeon, self.aggro, self.boss_berserk)
        };
        let n = self.community.len();
        if n <= 3 {
            return resolve(&self.community);
        }
        // 枚举所有「选 3 张」组合，取最优（通关优先，否则比队伍强度）。
        let mut best: Option<crate::combat::Report> = None;
        for i in 0..n {
            for j in (i + 1)..n {
                for k in (j + 1)..n {
                    let r = resolve(&[self.community[i], self.community[j], self.community[k]]);
                    if best.map_or(true, |b| r.pick_score() > b.pick_score()) {
                        best = Some(r);
                    }
                }
            }
        }
        best.expect("n>3 必有组合")
    }
}

#[derive(Debug, Clone)]
pub struct PlayerResult {
    pub id: PlayerId,
    pub cleared: bool,
    pub remaining_health: u32,
    pub remaining_power: u32,
    /// 战后优势值（赢家判定依据）。
    pub advantage: u32,
    /// 起始队伍战力 / 生命（计分板展示用，团灭时也有值）。
    pub team_power: u32,
    pub team_health: u32,
}

#[derive(Debug, Clone)]
pub struct Settlement {
    pub pot: u32,
    /// 主池赢家；None 表示无人通关，主池滚入下一局 jackpot。
    pub winner: Option<PlayerId>,
    pub results: Vec<PlayerResult>,
}

// ---- 占位牌库（之后替换为真正的卡牌设计）----

// art 名对应 assets/cards/community/<art>.png。角色 / 绑定装备 / 战术消耗品三类。
fn sample_community_deck() -> Vec<CommunityCard> {
    use Class::*;
    fn unit(class: Class, p: u32, h: u32, art: &'static str) -> CommunityCard {
        CommunityCard::Unit(Adventurer::new(class, p, h, art))
    }
    use CommunityCard::{Consum, Equip};
    vec![
        // 角色：每个职业各 3 张（5 职业 = 15 张），同职业多份方便触发对子/三条。
        // 战士（高血肉盾）
        unit(Warrior, 4, 12, "Warrior"),
        unit(Warrior, 5, 11, "Knight"),
        unit(Warrior, 6, 10, "Knight"),
        // 神官（治疗/高血）
        unit(Cleric, 3, 10, "Cleric"),
        unit(Cleric, 4, 9, "Cleric"),
        unit(Cleric, 3, 11, "Cleric"),
        // 法师（高战力脆皮）
        unit(Mage, 10, 4, "Mage"),
        unit(Mage, 9, 5, "Mage"),
        unit(Mage, 11, 3, "Mage"),
        // 盗贼（爆发）
        unit(Rogue, 8, 6, "Rogue"),
        unit(Rogue, 7, 7, "Rogue"),
        unit(Rogue, 9, 5, "Rogue"),
        // 游侠（远程均衡）
        unit(Ranger, 7, 7, "Archer"),
        unit(Ranger, 8, 6, "Scout"),
        unit(Ranger, 6, 9, "Archer"),
        // 绑定装备（需对应职业在场才生效）
        Equip(EquipKind::IronShield),  // 战士
        Equip(EquipKind::Dagger),      // 盗贼
        Equip(EquipKind::ArcaneStaff), // 法师
        Equip(EquipKind::Longbow),     // 游侠
        Equip(EquipKind::HolyChalice), // 神官
        // 战术 / 消耗品
        Consum(ConsumKind::Fireball),
        Consum(ConsumKind::HealingPotion),
        Consum(ConsumKind::Purify),
        Consum(ConsumKind::SmokeBomb),
    ]
}

/// 冒险者底牌库：每手洗牌后给每个座位发 2 张（允许重复）。
/// 只有 7 种立绘（对应 assets/cards/community/<art>.png 里的角色），
/// 这里给每种放数张，凑够 6 人×2=12 张还有富余。数值即平衡点，可调。
pub fn sample_adventurer_deck() -> Vec<Adventurer> {
    use Class::*;
    let kinds = [
        (Warrior, 5, 12, "Warrior"),
        (Warrior, 6, 11, "Knight"),
        (Cleric, 4, 10, "Cleric"),
        (Mage, 10, 4, "Mage"),
        (Mage, 9, 5, "Mage"),
        (Rogue, 8, 6, "Rogue"),
        (Ranger, 7, 7, "Archer"),
        (Ranger, 8, 6, "Scout"),
    ];
    let mut deck = Vec::with_capacity(kinds.len() * 3);
    for _ in 0..3 {
        for &(class, p, h, art) in &kinds {
            deck.push(Adventurer::new(class, p, h, art));
        }
    }
    deck
}

// art 名对应 assets/cards/dungeon/<art>.png。三档分别在 Flop/Turn/River 翻开。
fn sample_dungeon_decks() -> (Vec<Monster>, Vec<Monster>, Vec<Monster>) {
    use MonsterKind::*;
    // 一手要连过 小怪→精英→Boss 三关：队伍 power 需压过每关 threat，
    // health 逐关扣对应 threat。基础威胁总和约 5+8+12=25，搭得好的队（含对子/
    // 装备）约 40 战力 / 50 生命能稳过；惊动值高或 All-in 狂暴时会顶不住。
    let smalls = vec![
        Monster::new(Goblin, 4, 5, "Goblin"),
        Monster::new(Goblin, 5, 6, "Skeleton_Soldier"),
        Monster::new(Goblin, 6, 5, "Dire_Wolf"),
    ];
    let elites = vec![
        Monster::new(Elite, 8, 10, "Elite_Goblin-Chief"),
        Monster::new(Elite, 7, 11, "Elite_Mimic"),
        Monster::new(PoisonSwamp, 8, 12, "Elite_Swamp_Witch"),
    ];
    let bosses = vec![
        Monster::new(Boss, 13, 20, "Boss_Ancient_Dragon"),
        Monster::new(Boss, 12, 18, "Boss_Troll_King"),
        Monster::new(Boss, 11, 19, "Boss_lich"),
        Monster::new(Boss, 12, 19, "Boss_Corrupted_Tree_Lord"),
    ];
    (smalls, elites, bosses)
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
                    [Adventurer::new(Warrior, 4, 9, "Warrior"), Adventurer::new(Mage, 6, 3, "Mage")],
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
    fn team_caps_at_three_community_cards() {
        use Class::*;
        // 5 张同质公共牌(各 P5/H5)，队伍只能取其中 3 张：用满 5 张血会更高，
        // 用 3 张则余血固定 → 以此验证「最多取 3 张」。
        let hole = [Adventurer::new(Warrior, 0, 0, "Warrior"), Adventurer::new(Warrior, 0, 0, "Warrior")];
        let mut g = Game::new(demo_players(2, 200), 10, 5);
        g.community = (0..5).map(|_| CommunityCard::Unit(Adventurer::new(Warrior, 5, 5, "Warrior"))).collect();
        g.dungeon = vec![Monster::new(MonsterKind::Boss, 20, 20, "Boss")];
        g.scene = Scene::StoneArena; // 战力 +2
        // 取 3 张：5 个战士(2 底牌+3) → 三条 +12/+12。
        // 战力 = 0+0 + 15 + 12 + 2 = 29；生命 = 0 + 15 + 12 = 27；过 Boss(威胁20) 余血 = 7。
        let o = g.simulate_clear(&hole);
        assert!(o.cleared);
        assert_eq!(o.remaining_health, 7, "用 4/5 张会让余血更高，余血=7 证明只取了 3 张");
        assert_eq!(o.remaining_power, 29);
    }

    #[test]
    fn class_pair_and_triple_synergy() {
        use Class::*;
        // 两个法师 → 对子 +6/+6。
        let hole = [Adventurer::new(Mage, 5, 2, "Mage"), Adventurer::new(Mage, 5, 2, "Mage")];
        let mut g = Game::new(demo_players(2, 200), 10, 1);
        g.community = vec![]; // 0 张公共牌：只看底牌 + 搭配
        g.dungeon = vec![]; // 空地牢：必通关，便于读数值
        g.scene = Scene::StoneArena; // +2 战力
        let o = g.simulate_clear(&hole);
        // 战力 = 5+5 + 6(对子) + 2(场景) = 18；生命 = 2+2 + 6 = 10。
        assert_eq!(o.team_power, 18);
        assert_eq!(o.team_health, 10);
    }

    #[test]
    fn equipment_needs_bound_class() {
        use Class::*;
        let mut g = Game::new(demo_players(2, 200), 10, 1);
        g.dungeon = vec![];
        g.scene = Scene::Tomb; // flat (+1,-1)，固定可预期
        // 法杖给战士：不生效。
        let warriors = [Adventurer::new(Warrior, 3, 5, "Warrior"), Adventurer::new(Rogue, 3, 5, "Rogue")];
        g.community = vec![CommunityCard::Equip(EquipKind::ArcaneStaff)];
        let no = g.simulate_clear(&warriors);
        // 战力 = 3+3 +1(场景) = 7（法杖无效）。
        assert_eq!(no.team_power, 7);
        // 法杖给法师：+6 战力。
        let mage = [Adventurer::new(Mage, 3, 5, "Mage"), Adventurer::new(Rogue, 3, 5, "Rogue")];
        let yes = g.simulate_clear(&mage);
        assert_eq!(yes.team_power, 7 + 6, "队伍有法师，法杖应生效 +6 战力");
    }

    #[test]
    fn allin_berserk_adds_boss_threat() {
        use Class::*;
        // 两个战士 → 对子 +6/+6；战力/生命都留足够余量扛住狂暴威胁。
        let hole = [Adventurer::new(Warrior, 50, 45, "Warrior"), Adventurer::new(Warrior, 0, 0, "Warrior")];
        let mut g = Game::new(demo_players(2, 200), 10, 1);
        g.community = vec![];
        g.scene = Scene::StoneArena; // +2 战力
        g.dungeon = vec![Monster::new(MonsterKind::Boss, 20, 100, "Boss")];
        // 战力 = 50+6+2 = 58；生命 = 45+6 = 51。
        let calm = g.simulate_clear(&hole); // 余血 51-20 = 31
        g.boss_berserk = true; // 威胁 +10 → 30
        let rage = g.simulate_clear(&hole); // 余血 51-30 = 21
        assert!(calm.cleared && rage.cleared);
        assert_eq!(calm.remaining_health - rage.remaining_health, 10, "狂暴让 Boss 威胁 +10(20→30)");
    }

    #[test]
    fn small_allin_does_not_trigger_berserk() {
        // 只剩很少筹码被迫梭哈，不应触发 Boss 狂暴。
        let mut g = Game::new(demo_players(2, 1000), 10, 1);
        g.players[0].chips = 20; // 让 P0 只剩 20
        g.to_act = 0;
        g.apply(Action::AllIn).unwrap();
        assert!(!g.boss_berserk, "小额 all-in 不该触发狂暴");

        // 大额 all-in（≥500）才触发。
        let mut g2 = Game::new(demo_players(2, 1000), 10, 2);
        g2.to_act = 0;
        g2.apply(Action::AllIn).unwrap(); // P0 押上约 990
        assert!(g2.boss_berserk, "大额 all-in 应触发狂暴");
    }

    #[test]
    fn early_allfold_runs_out_full_board() {
        // 全员翻牌前弃到只剩一人：应把牌面跑满到 River —— 5 张公共牌 + 完整地牢
        // (小怪+精英+Boss)，独狼诈唬也得跑全程。
        let mut g = Game::new(demo_players(3, 200), 10, 3);
        g.apply(Action::Raise { to: 40 }).unwrap();
        g.apply(Action::Fold).unwrap();
        g.apply(Action::Fold).unwrap();
        assert_eq!(g.phase, Phase::Showdown);
        assert_eq!(g.community.len(), 5, "早摊牌应补满 5 张公共牌");
        assert_eq!(g.dungeon.len(), 3, "早摊牌应补齐完整地牢三档");
        // 地牢按 小怪→精英→Boss 顺序补齐。
        assert_eq!(g.dungeon[0].kind, MonsterKind::Goblin);
        assert_eq!(g.dungeon[2].kind, MonsterKind::Boss);
        let _ = g.settle(); // 不应 panic，能正常结算
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
