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

/// 装备：**绑定职业**——队伍里有对应职业才生效，否则当白板（鼓励组队搭配）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EquipKind {
    IronShield,  // 铁盾 → 战士：前排护盾，+生命
    Longbow,     // 长弓 → 游侠：远程输出，+战力
    HolyChalice, // 圣杯 → 神官：治疗增强，+生命
    Dagger,      // 匕首 → 盗贼：先手，+战力
    ArcaneStaff, // 法杖 → 法师：法术增强，+战力
}

impl EquipKind {
    /// 绑定的职业。
    pub fn class(&self) -> Class {
        match self {
            EquipKind::IronShield => Class::Warrior,
            EquipKind::Longbow => Class::Ranger,
            EquipKind::HolyChalice => Class::Cleric,
            EquipKind::Dagger => Class::Rogue,
            EquipKind::ArcaneStaff => Class::Mage,
        }
    }
    /// 队伍里有绑定职业时给的 (战力, 生命) 加成（平衡数值集中在这里）。
    pub fn bonus(&self) -> (u32, u32) {
        match self {
            EquipKind::IronShield => (0, 7),
            EquipKind::Longbow => (5, 0),
            EquipKind::HolyChalice => (0, 6),
            EquipKind::Dagger => (5, 0),
            EquipKind::ArcaneStaff => (6, 0),
        }
    }
    pub fn art(&self) -> &'static str {
        match self {
            EquipKind::IronShield => "Iron_Shield",
            EquipKind::Longbow => "Longbow",
            EquipKind::HolyChalice => "Holy_Chalice",
            EquipKind::Dagger => "Dagger",
            EquipKind::ArcaneStaff => "Arcane_Staff",
        }
    }
    /// 给牌面叠的短标注：绑定职业 + 加成。
    pub fn tag(&self) -> String {
        let (p, h) = self.bonus();
        let cls = match self.class() {
            Class::Warrior => "战",
            Class::Cleric => "神",
            Class::Mage => "法",
            Class::Rogue => "盗",
            Class::Ranger => "游",
        };
        format!("{}专 +{}P{}H", cls, p, h)
    }
}

/// 战术 / 消耗品：闯关时触发特定效果，让玩家在摊牌前有「我还有一张解法」的空间。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsumKind {
    Fireball,      // 火球术：削小怪威胁
    HealingPotion, // 治疗药剂：队伍 +生命，并算作「治疗」可免疫沼泽叠毒
    Purify,        // 净化术：解毒/解诅咒，免疫沼泽叠毒
    SmokeBomb,     // 烟雾弹：伏击，首个地牢节点减伤
}

impl ConsumKind {
    pub fn art(&self) -> &'static str {
        match self {
            ConsumKind::Fireball => "Fireball",
            ConsumKind::HealingPotion => "Healing_Potion",
            ConsumKind::Purify => "Purify",
            ConsumKind::SmokeBomb => "Smoke_Bomb",
        }
    }
    pub fn tag(&self) -> &'static str {
        match self {
            ConsumKind::Fireball => "火球·削小怪",
            ConsumKind::HealingPotion => "药剂·+生命",
            ConsumKind::Purify => "净化·免毒",
            ConsumKind::SmokeBomb => "烟雾·首节减伤",
        }
    }
}

/// 公共池的一张牌：角色 / 绑定装备 / 战术消耗品。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommunityCard {
    Unit(Adventurer),
    Equip(EquipKind),
    Consum(ConsumKind),
}

impl CommunityCard {
    /// 这张牌的立绘标识（community 目录下的文件名 stem）。
    pub fn art(&self) -> &'static str {
        match self {
            CommunityCard::Unit(a) => a.art,
            CommunityCard::Equip(k) => k.art(),
            CommunityCard::Consum(c) => c.art(),
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

/// 牌桌场景：每手随机一个，给闯关队伍挂不同 buff（正/负）。
/// `art` 对应 `assets/table/<art>.png`，表现层据此换背景。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scene {
    IceField,   // 冰原：迟缓，战力 -
    StoneArena, // 竞技场：战意，战力 +
    Marsh,      // 毒沼：生命 -
    Tomb,       // 墓地：亡灵气息，生命 -
    Volcano,    // 火山：输出 +，灼烧 -生命
    WheatField, // 麦田：休整，生命 +
}

impl Scene {
    pub const ALL: [Scene; 6] = [
        Scene::IceField,
        Scene::StoneArena,
        Scene::Marsh,
        Scene::Tomb,
        Scene::Volcano,
        Scene::WheatField,
    ];

    /// 背景图文件名 stem（assets/table/<art>.png）。
    pub fn art(&self) -> &'static str {
        match self {
            Scene::IceField => "bkg_Ice_field",
            Scene::StoneArena => "bkg_Stone_arena",
            Scene::Marsh => "bkg_marsh",
            Scene::Tomb => "bkg_tomb",
            Scene::Volcano => "bkg_volcano",
            Scene::WheatField => "bkg_wheat_field",
        }
    }

    /// 给玩家看的中文场景名 + 效果。
    pub fn label(&self) -> &'static str {
        match self {
            Scene::IceField => "冰原 (战力 -2)",
            Scene::StoneArena => "竞技场 (战力 +2)",
            Scene::Marsh => "毒沼 (无治疗则逐节点叠毒)",
            Scene::Tomb => "墓地 (战力 +1, 生命 -1)",
            Scene::Volcano => "火山 (每轮 Boss 惊动 +1)",
            Scene::WheatField => "麦田 (弓手/法师 +战力)",
        }
    }

    /// 简单场景对队伍的固定加成 (Δ战力, Δ生命)。
    /// 火山/毒沼/麦田是**动态/条件**效果，不在这里，分别在惊动推进与 combat 里处理。
    pub fn flat_buff(&self) -> (i32, i32) {
        match self {
            Scene::IceField => (-2, 0),
            Scene::StoneArena => (2, 0),
            Scene::Tomb => (1, -1),
            Scene::Volcano | Scene::Marsh | Scene::WheatField => (0, 0),
        }
    }
}
