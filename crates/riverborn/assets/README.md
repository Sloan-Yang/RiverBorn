# 美术素材目录

Bevy 运行时从**项目根目录的 `assets/`** 加载素材（因为 `cargo run -p riverborn`
的工作目录是 workspace 根）。代码里用 `asset_server.load("相对路径")`，路径就是
相对本目录。例如 `asset_server.load("table/background.png")`。

支持格式：PNG / JPG（背景大图建议 PNG 或 JPG 均可）。

文件名**必须**和下面的约定一致——代码会按枚举名拼路径来加载，名字对不上就读不到。

## 目录与命名约定

```
assets/
├── table/
│   └── background.png            牌桌背景（建议铺满窗口，当前窗口 2048×1080）
│
├── avatars/                      玩家/AI 形象（按玩家名小写、连字符转下划线）
│   ├── you.png                   人类玩家
│   ├── ai-gambler.png
│   ├── ai-rookie.png
│   └── ai-veteran.png
│
└── cards/
    ├── back.png                  卡背（盖住的牌、未翻开的公共/地牢牌、AI 底牌）
    │
    ├── community/                公共池的牌（按职业 / 装备）
    │   ├── warrior.png           ← Class::Warrior
    │   ├── cleric.png            ← Class::Cleric
    │   ├── mage.png              ← Class::Mage
    │   ├── rogue.png             ← Class::Rogue
    │   ├── ranger.png            ← Class::Ranger
    │   └── gear.png              ← CommunityCard::Gear（见下方说明）
    │
    └── dungeon/                  地牢/Boss 池的牌（按怪物种类）
        ├── goblin.png            ← MonsterKind::Goblin
        ├── poison_swamp.png      ← MonsterKind::PoisonSwamp
        ├── elite.png             ← MonsterKind::Elite
        ├── treasure.png          ← MonsterKind::Treasure
        └── boss.png              ← MonsterKind::Boss
```

## 说明

- **同职业共用一张图**：公共牌库里有两张战士、两张法师，它们共用 `warrior.png`、
  `mage.png`，数值不同但贴同一张图。
- **装备（gear.png）**：目前 `CommunityCard::Gear` 只有数值（加攻/加血），没有
  细分种类，所以先用一张通用 `gear.png`。等以后想让每件装备有不同图，需要先给
  卡牌数据加一个标识字段（告诉我，我来加）。
- **卡背 back.png** 一张图复用在所有"盖住"的位置：未翻开的公共/地牢牌、以及
  AI 玩家隐藏的底牌。
- **形象图命名**：按玩家 `name` 字段小写化得到（`"AI-Gambler"` → `ai-gambler.png`）。
  改了玩家名记得同步改文件名。

放好图后告诉我，我把加载代码接上（背景铺满、牌槽贴图、卡背、头像），
把现在的占位方框换成真正的图片。
```
