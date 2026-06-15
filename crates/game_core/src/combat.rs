//! 战斗结算：组队（职业搭配 + 绑定装备 + 消耗品 + 场景）→ 跑地牢。
//!
//! 队伍构成规则：**2 底牌 + 公共池里选中的 ≤3 张牌 + 场景效果**。
//! 组队时结算职业「对子/三条」搭配、绑定装备（需对应职业在场）、治疗/净化等
//! 消耗品；跑地牢时结算惊动值（精英/Boss 增强、All-in 让 Boss 狂暴）、火球削
//! 小怪、烟雾伏击、毒沼叠毒等。纯函数、确定性，可单测。
//!
//! 平衡数值全部集中为下面的常量，便于调。

use crate::cards::*;

// ---- 平衡常量（随便调）----
/// 同职业「对子」加成 (战力, 生命)。
const PAIR_BONUS: (i32, i32) = (6, 6);
/// 同职业「三条」加成（≥3 个）。
const TRIPLE_BONUS: (i32, i32) = (12, 12);
/// 麦田：弓手 / 法师每个单位的 +战力。
const WHEAT_RANGER_P: i32 = 3;
const WHEAT_MAGE_P: i32 = 3;
/// 治疗药剂的固定 +生命。
const HEAL_POTION_H: i32 = 6;
/// 每 1 点惊动值给精英/Boss 增加的威胁（向下取整）。
const AGGRO_THREAT_PER_POINT: f32 = 0.25;
/// 火球术对小怪节点的威胁削减。
const FIREBALL_REDUCE: i32 = 4;
/// 烟雾弹对首个地牢节点的威胁削减。
const SMOKE_REDUCE: i32 = 4;
/// 毒沼每叠一层毒，对队伍战力/生命各扣多少。
const POISON_PER_STACK: i32 = 1;
/// Boss 狂暴时额外增加的威胁（不再翻倍）。
const BERSERK_THREAT_BONUS: i32 = 10;

/// 闯关战报：起始队伍 P/H、是否通关、通关后剩余 P/H。
/// 团灭时也带队伍 P/H（用于结算计分板，让玩家看懂怎么团灭的）。
#[derive(Debug, Clone, Copy)]
pub struct Report {
    pub team_power: u32,
    pub team_health: u32,
    pub cleared: bool,
    pub remaining_health: u32,
    pub remaining_power: u32,
}

impl Report {
    /// 战后优势值：剩余生命 + 剩余战力（团灭为 0）。
    pub fn advantage(&self) -> u32 {
        if self.cleared {
            self.remaining_health + self.remaining_power
        } else {
            0
        }
    }
    /// 在多种「选 3 张」组合里挑最优时的排序分：通关优先，否则比起始队伍强度。
    pub fn pick_score(&self) -> u64 {
        if self.cleared {
            1_000_000 + self.advantage() as u64
        } else {
            (self.team_power + self.team_health) as u64
        }
    }
}

/// 组好的队伍 + 携带的战术能力。
struct Squad {
    power: u32,
    health: u32,
    has_cleanse: bool,  // 神官 / 净化 / 治疗 → 免疫毒沼叠毒
    has_fireball: bool, // 火球削小怪
    has_smoke: bool,    // 烟雾伏击首节点
}

/// 用「底牌 + 选中的公共牌 + 场景」组队并跑地牢，返回战报（含起始队伍 P/H）。
pub fn resolve(
    hole: &[Adventurer; 2],
    picks: &[CommunityCard],
    scene: Scene,
    dungeon: &[Monster],
    aggro: f32,
    berserk: bool,
) -> Report {
    let squad = build_squad(hole, picks, scene);
    let (team_power, team_health) = (squad.power, squad.health);
    match run_dungeon(squad, scene, dungeon, aggro, berserk) {
        Some((h, p)) => Report { team_power, team_health, cleared: true, remaining_health: h, remaining_power: p },
        None => Report { team_power, team_health, cleared: false, remaining_health: 0, remaining_power: 0 },
    }
}

/// 节点经惊动/狂暴调整后的**有效威胁**（不含队伍消耗品减免）。
/// 结算与计分板共用，保证显示的威胁和实际结算一致。
pub fn node_threat(node: &Monster, aggro: f32, berserk: bool) -> u32 {
    let mut t = node.threat as i32;
    if matches!(node.kind, MonsterKind::Elite | MonsterKind::PoisonSwamp | MonsterKind::Boss) {
        t += (aggro.max(0.0) * AGGRO_THREAT_PER_POINT) as i32;
    }
    if node.kind == MonsterKind::Boss && berserk {
        t += BERSERK_THREAT_BONUS;
    }
    t.max(0) as u32
}

/// 同职业搭配加成：对子 +6/+6，三条 +12/+12（每个职业各结算一次）。
fn class_synergy(units: &[Adventurer]) -> (i32, i32) {
    let mut p = 0;
    let mut h = 0;
    for cls in [Class::Warrior, Class::Cleric, Class::Mage, Class::Rogue, Class::Ranger] {
        let cnt = units.iter().filter(|u| u.class == cls).count();
        let (bp, bh) = if cnt >= 3 {
            TRIPLE_BONUS
        } else if cnt == 2 {
            PAIR_BONUS
        } else {
            (0, 0)
        };
        p += bp;
        h += bh;
    }
    (p, h)
}

fn build_squad(hole: &[Adventurer; 2], picks: &[CommunityCard], scene: Scene) -> Squad {
    // 单位 = 2 底牌 + 公共池里选中的角色。
    let mut units: Vec<Adventurer> = hole.to_vec();
    for c in picks {
        if let CommunityCard::Unit(a) = c {
            units.push(*a);
        }
    }

    let mut power: i32 = 0;
    let mut health: i32 = 0;
    for u in &units {
        power += u.power as i32;
        health += u.health as i32;
        // 麦田：弓手/法师 +战力。
        if scene == Scene::WheatField {
            match u.class {
                Class::Ranger => power += WHEAT_RANGER_P,
                Class::Mage => power += WHEAT_MAGE_P,
                _ => {}
            }
        }
    }

    // 职业搭配。
    let (sp, sh) = class_synergy(&units);
    power += sp;
    health += sh;

    // 装备（绑定职业才生效）+ 消耗品。
    let has_class = |c: Class| units.iter().any(|u| u.class == c);
    let mut has_cleanse = has_class(Class::Cleric); // 神官自带治疗
    let mut has_fireball = false;
    let mut has_smoke = false;
    for c in picks {
        match c {
            CommunityCard::Equip(k) => {
                if has_class(k.class()) {
                    let (p, h) = k.bonus();
                    power += p as i32;
                    health += h as i32;
                }
            }
            CommunityCard::Consum(k) => match k {
                ConsumKind::HealingPotion => {
                    health += HEAL_POTION_H;
                    has_cleanse = true;
                }
                ConsumKind::Purify => has_cleanse = true,
                ConsumKind::Fireball => has_fireball = true,
                ConsumKind::SmokeBomb => has_smoke = true,
            },
            CommunityCard::Unit(_) => {}
        }
    }

    // 场景固定加成（冰原/竞技场/墓地）。
    let (dp, dh) = scene.flat_buff();
    power += dp;
    health += dh;

    Squad {
        power: power.max(0) as u32,
        health: health.max(0) as u32,
        has_cleanse,
        has_fireball,
        has_smoke,
    }
}

/// 逐节点跑地牢。返回通关后 (剩余生命, 剩余战力)，团灭返回 None。
fn run_dungeon(squad: Squad, scene: Scene, dungeon: &[Monster], aggro: f32, berserk: bool) -> Option<(u32, u32)> {
    let mut power = squad.power as i32;
    let mut health = squad.health as i32;
    let mut poison = 0i32;

    for (i, node) in dungeon.iter().enumerate() {
        // 毒沼场景：无治疗时逐节点叠毒，扣队伍战力/生命。
        if scene == Scene::Marsh && !squad.has_cleanse {
            poison += POISON_PER_STACK;
            power -= poison;
            health -= poison;
        }

        // 宝箱：无威胁，跳过。
        if node.kind == MonsterKind::Treasure {
            continue;
        }

        // 节点有效威胁（惊动/狂暴）再叠队伍消耗品减免（火球削小怪、烟雾伏击首节点）。
        let mut threat = node_threat(node, aggro, berserk) as i32;
        if squad.has_fireball && node.kind == MonsterKind::Goblin {
            threat -= FIREBALL_REDUCE;
        }
        if squad.has_smoke && i == 0 {
            threat -= SMOKE_REDUCE;
        }
        let threat = threat.max(0);

        // 毒沼节点：克制低生命队伍，额外掉血。
        if node.kind == MonsterKind::PoisonSwamp && health < node.health as i32 {
            health -= node.threat as i32 / 2;
        }

        // 战力压不过威胁 → 团灭；承伤扛不住 → 团灭。
        if power < threat || health <= threat {
            return None;
        }
        health -= threat;
    }

    Some((health.max(0) as u32, power.max(0) as u32))
}
