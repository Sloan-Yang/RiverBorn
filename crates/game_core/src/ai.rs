//! 单机对战的启发式 AI。
//!
//! 思路贴合游戏的博弈核心：AI 用 [`Game::simulate_clear`] 预演
//! 「自己底牌 + 当前已揭示公共池」能否打过当前地牢、能剩多少血，
//! 再结合「现在需要跟多少注」决定 Check/Call/Raise/Fold。
//!
//! 纯函数、确定性，可单元测试。含**诈唬**：弱牌也会偶尔加注施压，
//! 并据「要跟的注 vs 底池」（赔率 / 对手下注力度）调节弃牌倾向。

use crate::game::{Action, Game, Phase};
use crate::player::Player;
use crate::rng::Rng;

/// 弱牌偷池（无人下注时主动加注诈唬）的概率（百分比）。
const STEAL_PCT: u64 = 15;
/// 弱牌面对小注时赖着不走（float 诈唬）的概率。
const FLOAT_PCT: u64 = 10;
/// 加注步长。
const RAISE_STEP: u32 = 20;
/// 战后优势值阈值：达到「强」才会主动价值加注 / 押全跟。
const STRONG_ADV: u32 = 25;
/// 达到「尚可」才愿意为看牌跟中等大小的注。
const DECENT_ADV: u32 = 12;

/// 为索引 `idx` 的玩家给出一个**合法**动作。
///
/// 思路：先用 [`Game::simulate_clear`] 估当前阵容能否过地牢、战后优势值多高。
/// - 过不了 → 保守（白嫖就过牌，要花钱基本弃，只偶尔小诈唬）。
/// - 过得了 → **手牌越强，加注概率越高**；但有「下注纪律」防止无限对加把钱打光，
///   且**不会为了加注主动梭哈**（只有跟注必须押上全部筹码且牌很强时才 all-in）。
pub fn decide(game: &Game, idx: usize) -> Action {
    let p = &game.players[idx];
    let need = game.current_bet.saturating_sub(p.committed); // 跟注还差多少
    let roll = bluff_roll(game, idx); // 0..100，按局面确定性产生

    // —— 估牌：打不过地牢 → 保守（含一点诈唬）——
    let report = game.simulate_clear(&p.hole);
    if !report.cleared {
        if need == 0 {
            if game.phase != Phase::PreFlop && roll < STEAL_PCT {
                return raise_or(game, p, need, Action::Check); // 偶尔偷池诈唬
            }
            return Action::Check;
        }
        if need <= p.chips / 10 && roll < FLOAT_PCT {
            return Action::Call; // 注很小，赖一手看能不能诈成
        }
        return Action::Fold; // 打不过又要花钱，撤
    }

    // —— 能过：手牌越强，加注概率越高 ——
    let strength = report.advantage();
    let mut raise_chance = strength_to_raise_chance(strength);
    if game.phase == Phase::PreFlop {
        raise_chance /= 2; // 翻牌前信息少，收一半
    }
    // 下注纪律：本轮下注线已吃掉这家约 1/3 筹码时就不再加注，避免无限对加把钱打光。
    let stack = p.chips + p.committed;
    let bet_heavy = game.current_bet.saturating_mul(3) >= stack;

    if need == 0 {
        // 没人下注：按概率价值下注，否则过牌。
        if !bet_heavy && roll < raise_chance {
            return raise_or(game, p, need, Action::Check);
        }
        return Action::Check;
    }

    // 面对下注：强牌按概率价值加注。
    if !bet_heavy && strength >= STRONG_ADV && roll < raise_chance {
        return raise_or(game, p, need, Action::Call);
    }
    // 跟注判定：赔率好（注相对底池便宜）、牌够强、或注很小，就跟；否则弃。
    let pot_odds_ok = need.saturating_mul(3) <= game.pot.max(1);
    if need < p.chips {
        if pot_odds_ok || strength >= DECENT_ADV || need <= p.chips / 5 {
            return Action::Call;
        }
        return Action::Fold;
    }
    // 跟注要押上全部筹码：只有强牌才 all-in，否则弃。
    if strength >= STRONG_ADV {
        Action::AllIn
    } else {
        Action::Fold
    }
}

/// 把战后优势值映射成加注概率（百分比）：牌越大越敢加。
fn strength_to_raise_chance(adv: u32) -> u64 {
    match adv {
        0..=11 => 8,
        12..=24 => 22,
        25..=39 => 42,
        40..=59 => 60,
        _ => 72,
    }
}

/// 试图加注一个步长；加不起时退回 `fallback`（fallback 为 Call 时需跟得起，否则弃）。
fn raise_or(game: &Game, p: &Player, need: u32, fallback: Action) -> Action {
    let target = game.current_bet + RAISE_STEP;
    if p.chips > 0 && target.saturating_sub(p.committed) <= p.chips {
        return Action::Raise { to: target };
    }
    match fallback {
        Action::Call if need <= p.chips => Action::Call,
        Action::Call => Action::Fold,
        other => other,
    }
}

/// 由当前局面确定性地产生一个 0..100 的诈唬掷骰。
/// 同一决策点结果稳定（不会反复横跳），换局面才变。
fn bluff_roll(game: &Game, idx: usize) -> u64 {
    let p = &game.players[idx];
    let seed = (game.pot as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ (idx as u64).wrapping_mul(0xD1B5_4A32_D192_ED03)
        ^ ((game.community.len() as u64) << 17)
        ^ ((game.current_bet as u64) << 5)
        ^ (p.chips as u64).wrapping_mul(0x2545_F491_4F6C_DD1D);
    Rng::new(seed).next_u64() % 100
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cards::{Adventurer, Class};
    use crate::player::Player;

    fn players() -> Vec<Player> {
        use Class::*;
        vec![
            Player::new(0, "AI0", true, 200, [Adventurer::new(Warrior, 4, 9, "Warrior"), Adventurer::new(Cleric, 2, 6, "Cleric")]),
            Player::new(1, "AI1", true, 200, [Adventurer::new(Mage, 6, 3, "Mage"), Adventurer::new(Rogue, 5, 4, "Rogue")]),
            Player::new(2, "AI2", true, 200, [Adventurer::new(Ranger, 4, 5, "Archer"), Adventurer::new(Warrior, 3, 8, "Knight")]),
        ]
    }

    /// 打不过地牢又面对一笔像样的注 → 必须弃牌（保守），不会乱加注。
    #[test]
    fn weak_hand_folds_to_a_real_bet() {
        use crate::cards::{Monster, MonsterKind};
        let mut g = Game::new(players(), 10, 1);
        g.players[0].hole = [Adventurer::new(Class::Mage, 0, 0, "Mage"), Adventurer::new(Class::Mage, 0, 0, "Mage")];
        g.community = vec![];
        g.dungeon = vec![Monster::new(MonsterKind::Boss, 50, 50, "Boss")]; // 远超弱队战力
        g.current_bet = 60;
        g.players[0].committed = 0;
        g.to_act = 0;
        assert!(!g.simulate_clear(&g.players[0].hole).cleared, "构造应使其打不过");
        assert_eq!(decide(&g, 0), Action::Fold, "打不过又面对大注，应弃牌");
    }

    /// 全 AI 自动对局：每个决策都必须是合法动作，且能正常走到摊牌。
    #[test]
    fn ai_only_table_reaches_showdown_with_legal_actions() {
        let mut g = Game::new(players(), 10, 2024);
        let mut guard = 0;
        while g.phase != Phase::Showdown && guard < 200 {
            let idx = g.to_act;
            let action = decide(&g, idx);
            g.apply(action).expect("AI 给出的动作必须合法");
            guard += 1;
        }
        assert_eq!(g.phase, Phase::Showdown, "AI 对局应能推进到摊牌");
        let s = g.settle();
        assert_eq!(s.pot, g.pot);
    }
}
