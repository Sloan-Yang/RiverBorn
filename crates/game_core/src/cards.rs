//! 卡牌与单位的纯数据定义。对应 README 的「底牌 / 公共池 / 怪物池」。
//!
//! 每张牌都带一个 `art` 美术标识（图片文件名，不含扩展名和目录），
//! 表现层按它去 assets 里找图。逻辑数值与美术解耦：同一个 `Class`
//! 可以有多张不同立绘（如 Ranger 对应 Archer / Scout）。

/// 冒险者职业。决定逻辑分类（未来的技能/克制用），立绘由 `art` 决定。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    Warrior, // 战士：高血量、嘲讽
    Cleric,  // 神官：治疗
    Mage,    // 法师：高伤害、脆皮
    Rogue,   // 盗贼：爆发
    Ranger,  // 游侠：远程
}

/// 一个冒险者单位。底牌的 2 张就是 `Adventurer`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Adventurer {
    pub class: Class,
    pub power: u32,  // 战力（贡献队伍输出）
    pub health: u32, // 生命（队伍承伤）
    /// 立绘：对应 `assets/cards/community/<art>.png`。
    pub art: &'static str,
}

impl Adventurer {
    pub fn new(class: Class, power: u32, health: u32, art: &'static str) -> Self {
        Self { class, power, health, art }
    }
}

/// 公共池的一张牌：角色，或给队伍加成的「装备 / 技能 / 药剂」。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommunityCard {
    Unit(Adventurer),
    /// 装备/技能/药剂：加总战力/生命。art 对应 `assets/cards/community/<art>.png`。
    Gear {
        bonus_power: u32,
        bonus_health: u32,
        art: &'static str,
    },
}

impl CommunityCard {
    /// 这张牌的立绘标识（community 目录下的文件名 stem）。
    pub fn art(&self) -> &'static str {
        match self {
            CommunityCard::Unit(a) => a.art,
            CommunityCard::Gear { art, .. } => art,
        }
    }
}

/// 地牢节点的档位：小怪 / 环境 / 精英 / 宝箱 / Boss。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MonsterKind {
    Goblin,      // 小怪
    PoisonSwamp, // 环境：克制低生命队伍
    Elite,       // 精英
    Treasure,    // 宝箱：威胁为 0
    Boss,        // 关底 Boss
}

/// 地牢里的一个节点。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Monster {
    pub kind: MonsterKind,
    pub threat: u32, // 威胁值：队伍战力需要压过它
    pub health: u32, // 节点血量
    /// 立绘：对应 `assets/cards/dungeon/<art>.png`。
    pub art: &'static str,
}

impl Monster {
    pub fn new(kind: MonsterKind, threat: u32, health: u32, art: &'static str) -> Self {
        Self { kind, threat, health, art }
    }
}
