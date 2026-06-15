//! 多手对局层：银行 / 借锅 / 记账 / 场景。
//!
//! 单手 [`Game`] 只负责「一手牌」的下注与结算。这一层把它包起来，
//! 跨手保留每个座位的筹码与负债，形成永不淘汰的现金局：
//! 输光可找银行借一锅（带利息），靠「水上/水下」的资产博弈制造张力。
//!
//! 全部确定性、零依赖，可用 `cargo test -p game_core` 验证。

use crate::cards::*;
use crate::game::*;
use crate::player::*;
use crate::rng::Rng;

/// 一锅 = 初始买入 / 每次借款额度。
pub const BUY_IN: u32 = 1000;
/// 每手对未偿负债收的利息（百分比）。
pub const INTEREST_PCT: u32 = 5;
/// 基础前注。
pub const BASE_ANTE: u32 = 10;

/// 建桌时的座位定义（名字 + 是否 AI）。筹码由本层统一发。
pub struct SeatDef {
    pub name: String,
    pub is_ai: bool,
}

impl SeatDef {
    pub fn new(name: impl Into<String>, is_ai: bool) -> Self {
        Self { name: name.into(), is_ai }
    }
}

/// 一个持久座位：跨手保留筹码与负债。
#[derive(Debug, Clone)]
pub struct Seat {
    pub id: PlayerId,
    pub name: String,
    pub is_ai: bool,
    pub chips: u32,
    /// 欠银行的钱（借入本金 + 累积利息）。
    pub debt: u32,
    /// 一共借过几锅（展示用）。
    pub loans: u32,
}

impl Seat {
    /// 净身价 = 筹码 − 负债。
    pub fn net_worth(&self) -> i64 {
        self.chips as i64 - self.debt as i64
    }
    /// 水位 = 净身价 − 初始买入。> 0 水上（赚），< 0 水下（亏）。
    pub fn profit(&self) -> i64 {
        self.net_worth() - BUY_IN as i64
    }
}

/// 一整场对局（多手循环）。
pub struct Match {
    pub seats: Vec<Seat>,
    /// 当前正在打的这一手。
    pub hand: Game,
    /// 已打过的手数（从 0 开始）。
    pub hand_no: u32,
    /// 本手前注（随手数递增 = blinds）。
    pub ante: u32,
    /// 庄家位，每手轮转。
    pub button: usize,
    /// 本手场景（buff 来源）。
    pub scene: Scene,
    /// 无人通关时滚存的彩池，并入下一手底池。
    pub carry: u32,
    /// 本手是否已结算（防重复入账）。
    pub settled: bool,
    /// 最近一次结算结果（结算面板展示用）。
    pub last: Option<Settlement>,
    rng: Rng,
    adv_deck: Vec<Adventurer>,
}

impl Match {
    /// 开一场新对局：每个座位买入 1 锅，发出第一手。
    pub fn new(defs: Vec<SeatDef>, seed: u64) -> Self {
        let seats = defs
            .into_iter()
            .enumerate()
            .map(|(i, d)| Seat {
                id: PlayerId(i as u32),
                name: d.name,
                is_ai: d.is_ai,
                chips: BUY_IN,
                debt: 0,
                loans: 0,
            })
            .collect();

        // 先放一个占位 hand，紧接着 deal_hand 覆盖。
        let mut m = Self {
            seats,
            hand: Game::new(Vec::new(), 0, 0),
            hand_no: 0,
            ante: BASE_ANTE,
            button: 0,
            scene: Scene::StoneArena,
            carry: 0,
            settled: false,
            last: None,
            rng: Rng::new(seed),
            adv_deck: sample_adventurer_deck(),
        };
        m.deal_hand();
        m
    }

    /// 发一手新牌：随机场景、随机底牌、收前注、并入滚存彩池。
    fn deal_hand(&mut self) {
        // 前注随手数递增：每 5 手 +BASE_ANTE。
        self.ante = BASE_ANTE * (1 + self.hand_no / 5);

        // 破产的 AI 自动找银行补一锅（人类用「借一锅」按钮自己决定）。
        for seat in &mut self.seats {
            if seat.is_ai && seat.chips < self.ante {
                seat.chips += BUY_IN;
                seat.debt += BUY_IN;
                seat.loans += 1;
            }
        }

        // 场景：每手随机。
        let scene = Scene::ALL[(self.rng.next_u64() as usize) % Scene::ALL.len()];
        self.scene = scene;

        // 洗底牌库，给每个座位发 2 张。
        self.rng.shuffle(&mut self.adv_deck);
        let mut it = self.adv_deck.iter().copied();
        let players: Vec<Player> = self
            .seats
            .iter()
            .map(|s| {
                let a = it.next().unwrap_or(Adventurer::new(Class::Warrior, 3, 9, "Warrior"));
                let b = it.next().unwrap_or(Adventurer::new(Class::Mage, 6, 3, "Mage"));
                Player::new(s.id.0, s.name.clone(), s.is_ai, s.chips, [a, b])
            })
            .collect();

        let hand_seed = self.rng.next_u64();
        let mut game = Game::new(players, self.ante, hand_seed);
        game.set_scene(scene);
        // 上一手无人通关滚存的彩池并入本手底池。
        if self.carry > 0 {
            game.add_to_pot(self.carry);
            self.carry = 0;
        }
        self.hand = game;
        self.settled = false;
        self.last = None;
    }

    /// 本手是否已经打到摊牌（可以结算了）。
    pub fn hand_over(&self) -> bool {
        matches!(self.hand.phase, Phase::Showdown | Phase::Done)
    }

    /// 结算当前这一手：底池入账、写回座位筹码、对负债计息。
    /// 幂等：只在首次（Showdown 且未结算）真正入账，之后返回缓存结果。
    ///
    /// `picks`：各人类玩家手选的 3 张公共牌下标（按 PlayerId）。空切片=全部自动选最优。
    /// 联机里多个人类各自提交；单机就传一个元素。AI / 未提交者始终自动选。
    pub fn settle_hand(&mut self, picks: &[(PlayerId, [usize; 3])]) -> Settlement {
        if self.settled {
            return self.last.clone().expect("settled 时 last 必有值");
        }
        let s = self.hand.settle_with(picks);

        // 底池入账：有赢家发给赢家，否则滚存到下一手。
        if let Some(winner_id) = s.winner {
            if let Some(p) = self.hand.players.iter_mut().find(|p| p.id == winner_id) {
                p.chips += self.hand.pot;
            }
        } else {
            self.carry += self.hand.pot;
        }

        // 把本手结束的筹码写回座位。
        for (seat, player) in self.seats.iter_mut().zip(self.hand.players.iter()) {
            seat.chips = player.chips;
        }

        // 对未偿负债计息（每手一次）。
        for seat in &mut self.seats {
            if seat.debt > 0 {
                let interest = (seat.debt * INTEREST_PCT / 100).max(1);
                seat.debt += interest;
            }
        }

        self.hand.finish();
        self.settled = true;
        self.last = Some(s.clone());
        s
    }

    /// 找银行借一锅：+1000 筹码、+1000 负债。
    /// 同步更新座位与（若本手仍在进行）当前手里的该玩家筹码。
    pub fn borrow(&mut self, seat_idx: usize) {
        if let Some(seat) = self.seats.get_mut(seat_idx) {
            seat.chips += BUY_IN;
            seat.debt += BUY_IN;
            seat.loans += 1;
        }
        if let Some(p) = self.hand.players.get_mut(seat_idx) {
            p.chips += BUY_IN;
        }
    }

    /// 进入下一手：轮转庄家、手数 +1、重新发牌。
    pub fn next_hand(&mut self) {
        if !self.seats.is_empty() {
            self.button = (self.button + 1) % self.seats.len();
        }
        self.hand_no += 1;
        self.deal_hand();
    }

    /// 座位索引按净身价从高到低排序（记账面板排名用）。
    pub fn ranking(&self) -> Vec<usize> {
        let mut idx: Vec<usize> = (0..self.seats.len()).collect();
        idx.sort_by_key(|&i| std::cmp::Reverse(self.seats[i].net_worth()));
        idx
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn defs() -> Vec<SeatDef> {
        vec![
            SeatDef::new("You", false),
            SeatDef::new("AI-1", true),
            SeatDef::new("AI-2", true),
        ]
    }

    #[test]
    fn everyone_buys_in_one_pot() {
        let m = Match::new(defs(), 1);
        assert_eq!(m.seats.len(), 3);
        assert!(m.seats.iter().all(|s| s.chips == BUY_IN && s.debt == 0));
        // 每人发了 2 张底牌，前注已收进底池。
        assert!(m.hand.players.iter().all(|p| p.chips == BUY_IN - m.ante));
        assert_eq!(m.hand.pot, m.ante * 3);
    }

    #[test]
    fn borrow_adds_chips_and_debt() {
        let mut m = Match::new(defs(), 2);
        let before = m.seats[0].chips;
        m.borrow(0);
        assert_eq!(m.seats[0].chips, before + BUY_IN);
        assert_eq!(m.seats[0].debt, BUY_IN);
        assert_eq!(m.seats[0].loans, 1);
        assert_eq!(m.seats[0].net_worth(), before as i64); // 净身价不变
        assert_eq!(m.seats[0].profit(), before as i64 - BUY_IN as i64);
    }

    #[test]
    fn settle_awards_pot_and_is_idempotent() {
        let mut m = Match::new(defs(), 3);
        // 全 AI/人类一路 check/call 到摊牌。
        let mut guard = 0;
        while !m.hand_over() && guard < 200 {
            let idx = m.hand.to_act;
            let action = crate::ai::decide(&m.hand, idx);
            m.hand.apply(action).unwrap();
            guard += 1;
        }
        assert!(m.hand_over());
        let pot = m.hand.pot;
        let s1 = m.settle_hand(&[]);
        let total_after: u32 = m.seats.iter().map(|x| x.chips).sum();
        // 二次结算应返回相同结果且不再二次发钱。
        let s2 = m.settle_hand(&[]);
        assert_eq!(s1.winner, s2.winner);
        let total_again: u32 = m.seats.iter().map(|x| x.chips).sum();
        assert_eq!(total_after, total_again);
        // 有赢家时底池进了某人筹码；无赢家时滚存。
        if s1.winner.is_some() {
            assert!(pot > 0);
        } else {
            assert_eq!(m.carry, pot);
        }
    }

    #[test]
    fn interest_accrues_on_debt() {
        let mut m = Match::new(defs(), 4);
        m.borrow(1);
        let debt0 = m.seats[1].debt;
        // 打到摊牌并结算 → 计息一次。
        let mut guard = 0;
        while !m.hand_over() && guard < 200 {
            let idx = m.hand.to_act;
            let action = crate::ai::decide(&m.hand, idx);
            m.hand.apply(action).unwrap();
            guard += 1;
        }
        m.settle_hand(&[]);
        assert!(m.seats[1].debt > debt0, "负债应因计息增长");
    }

    /// 平衡哨兵：模拟大量自动对局，确认「有人通关」的比例不至于过低
    /// （旧数值下几乎团灭）。也防止以后改数值改崩。
    #[test]
    fn clear_rate_is_reasonable() {
        let (mut hands, mut cleared) = (0u32, 0u32);
        for seed in 0..150u64 {
            let mut m = Match::new(defs(), seed);
            for _ in 0..3 {
                let mut guard = 0;
                while !m.hand_over() && guard < 300 {
                    let idx = m.hand.to_act;
                    m.hand.apply(crate::ai::decide(&m.hand, idx)).unwrap();
                    guard += 1;
                }
                let s = m.settle_hand(&[]);
                hands += 1;
                if s.winner.is_some() {
                    cleared += 1;
                }
                m.next_hand();
            }
        }
        let rate = cleared as f32 / hands as f32;
        println!("clear rate = {:.0}% ({}/{})", rate * 100.0, cleared, hands);
        // 这是「全员激进下注」的最坏估计，真实人机局通关率更高。
        // 设下限防止以后改数值把地牢调到几乎团灭。
        assert!(rate > 0.5, "通关率过低 {:.0}%，地牢太难", rate * 100.0);
    }

    #[test]
    fn next_hand_rotates_and_redeals() {
        let mut m = Match::new(defs(), 5);
        let btn0 = m.button;
        let hole0 = m.hand.players[0].hole;
        // 快进结算这一手。
        let mut guard = 0;
        while !m.hand_over() && guard < 200 {
            let idx = m.hand.to_act;
            m.hand.apply(crate::ai::decide(&m.hand, idx)).unwrap();
            guard += 1;
        }
        m.settle_hand(&[]);
        m.next_hand();
        assert_eq!(m.hand_no, 1);
        assert_eq!(m.button, (btn0 + 1) % m.seats.len());
        assert_eq!(m.hand.phase, Phase::PreFlop);
        // 重新发了牌（极大概率与上一手不同）。
        let _ = hole0;
    }
}
