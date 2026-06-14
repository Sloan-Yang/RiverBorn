//! 单机对战的启发式 AI。
//!
//! 思路贴合游戏的博弈核心：AI 用 [`Game::simulate_clear`] 预演
//! 「自己底牌 + 当前已揭示公共池」能否打过当前地牢、能剩多少血，
//! 再结合「现在需要跟多少注」决定 Check/Call/Raise/Fold。
//!
//! 纯函数、确定性，可单元测试。先不做 bluff（诈唬），等基础手感对了再加。

use crate::game::{Action, Game, Phase};

/// 为索引 `idx` 的玩家给出一个**合法**动作。
pub fn decide(game: &Game, idx: usize) -> Action {
    let p = &game.players[idx];
    let need = game.current_bet.saturating_sub(p.committed); // 跟注还差多少

    match game.simulate_clear(&p.hole) {
        // 预计打不过当前地牢：有人下注就撤退，没人下注就白嫖看下一张。
        None => {
            if need > 0 {
                Action::Fold
            } else {
                Action::Check
            }
        }
        // 预计能通关，余血 hp 越高越敢压。
        Some(hp) => {
            // Pre-flop 信息太少（地牢还没翻），一律保守，不主动加注。
            let confident = hp >= 6 && game.phase != Phase::PreFlop;
            if confident {
                let target = game.current_bet + 20;
                if target.saturating_sub(p.committed) <= p.chips {
                    Action::Raise { to: target } // 价值加注
                } else if need <= p.chips {
                    Action::Call
                } else {
                    Action::AllIn
                }
            } else if need == 0 {
                Action::Check
            } else if need <= p.chips / 4 {
                Action::Call // 注不大，跟着看牌
            } else {
                Action::Fold // 注太大又没把握，撤
            }
        }
    }
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
