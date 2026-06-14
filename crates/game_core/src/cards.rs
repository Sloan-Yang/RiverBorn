//! 卡牌与单位的纯数据定义。对应 README 的「底牌 / 公共池 / 怪物池」。

/// 冒险者职业。底牌和公共池里的角色卡都用它。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    Warrior, // 战士：高血量、嘲讽（吸引怪物火力）
    Cleric,  // 神官：治疗
    Mage,    // 法师：高伤害、脆皮
    Rogue,   // 盗贼：爆发
    Ranger,  // 游侠：远程稳定输出
}

/// 一个冒险者单位。底牌的 2 张就是 `Adventurer`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Adventurer {
    pub class: Class,
    pub power: u32,  // 战力（贡献队伍输出）
    pub health: u32, // 生命（队伍承伤）
}

impl Adventurer {
    pub fn new(class: Class, power: u32, health: u32) -> Self {
        Self { class, power, health }
    }
}

/// 公共池的一张牌。README 里公共池是「角色/装备/技能/消耗品/费用位」，
/// 这里先实现「角色」和「装备」两类，足够跑通构筑逻辑。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommunityCard {
    Unit(Adventurer),
    /// 装备：给整支队伍加成的 buff（简化为加总战力/生命）。
    Gear { bonus_power: u32, bonus_health: u32 },
}

/// 地牢节点：小怪 / 环境事件 / Boss。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MonsterKind {
    Goblin,      // 小怪
    PoisonSwamp, // 环境：克制低生命队伍
    Elite,       // 精英
    Treasure,    // 宝箱：威胁为 0，但有收益（先留接口）
    Boss,        // 关底 Boss
}

/// 地牢里的一个节点。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Monster {
    pub kind: MonsterKind,
    pub threat: u32, // 威胁值：队伍战力需要压过它
    pub health: u32, // 节点血量：决定能否在限定回合内打穿
}

impl Monster {
    pub fn new(kind: MonsterKind, threat: u32, health: u32) -> Self {
        Self { kind, threat, health }
    }
}
