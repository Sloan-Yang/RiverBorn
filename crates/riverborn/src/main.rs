//! RiverBorn —— Bevy 表现层（单机对 AI 起步骨架）。
//!
//! 牌桌布局：6 个座位环绕四周，中间公共池，下方 Boss/地牢池，每座位面前两张
//! 底牌位。素材来自 assets/：背景、卡背每局随机；公共牌/地牢牌/底牌按各自的
//! 美术 id 贴图，未翻开的牌显示卡背。规则全在 game_core，这一层只做「显示 + 输入」。

use bevy::audio::{AudioPlayer, AudioSource, PlaybackSettings};
use bevy::prelude::*;
use game_core::{Action, Adventurer, Class, Game, Phase, Player};

/// 把整局游戏作为一个 Bevy 资源持有。
#[derive(Resource)]
struct GameSession {
    game: Game,
}

/// 音频资源：背景音乐循环播放，动作音效在合法行动后触发。
#[derive(Resource, Clone)]
struct AudioAssets {
    bgm: Handle<AudioSource>,
    check: Handle<AudioSource>,
    fold: Handle<AudioSource>,
    raise: Handle<AudioSource>,
}

/// 本局选中的卡背（盖住的牌都用它）。
#[derive(Resource)]
struct CardBack(Handle<Image>);

/// 每个会被刷新的位置，用这个枚举标明它代表牌桌上的哪个槽。
#[derive(Component)]
enum Slot {
    Status,                           // 左上角状态栏（文字）
    SeatName(usize),                  // 第 i 个座位的玩家名（文字）
    Community(usize),                 // 公共池第 i 张（图片）
    Dungeon(usize),                   // 地牢/Boss 池第 i 个节点（图片）
    Hole { seat: usize, idx: usize }, // 第 i 个座位的第 idx 张底牌（图片）
}

// 边框配色
const COL_DIM: Color = Color::srgb(0.30, 0.32, 0.38);    // 未翻开/空位
const COL_GOLD: Color = Color::srgb(0.85, 0.72, 0.35);   // 公共池
const COL_RED: Color = Color::srgb(0.85, 0.35, 0.35);    // 地牢/Boss
const COL_HOLE: Color = Color::srgb(0.55, 0.58, 0.65);   // 底牌
const COL_ACTIVE: Color = Color::srgb(0.40, 0.85, 0.45); // 当前行动高亮

/// 牌桌尺寸（窗口逻辑像素）。
const W: f32 = 2048.0;
const H: f32 = 1080.0;

/// 卡牌素材原始宽高比：911 / 1727。
const CARD_ASPECT: f32 = 911.0 / 1727.0;

const COMMUNITY_CARD_H: f32 = 180.0;
const COMMUNITY_CARD_W: f32 = COMMUNITY_CARD_H * CARD_ASPECT;
const COMMUNITY_CARD_GAP: f32 = 16.0;
const COMMUNITY_POOL_W: f32 = COMMUNITY_CARD_W * 5.0 + COMMUNITY_CARD_GAP * 4.0;
const COMMUNITY_POOL_X: f32 = (W - COMMUNITY_POOL_W) / 2.0;
const COMMUNITY_POOL_Y: f32 = 330.0;

const DUNGEON_CARD_H: f32 = 180.0;
const DUNGEON_CARD_W: f32 = DUNGEON_CARD_H * CARD_ASPECT;
const DUNGEON_CARD_GAP: f32 = 20.0;
const DUNGEON_POOL_W: f32 = DUNGEON_CARD_W * 3.0 + DUNGEON_CARD_GAP * 2.0;
const DUNGEON_POOL_X: f32 = (W - DUNGEON_POOL_W) / 2.0;
const DUNGEON_POOL_Y: f32 = 560.0;

const HOLE_CARD_H: f32 = 132.0;
const HOLE_CARD_W: f32 = HOLE_CARD_H * CARD_ASPECT;
const HOLE_CARD_GAP: f32 = 10.0;
const HOLE_CARD_TOP_OFFSET: f32 = 96.0;

const AVATAR_SIZE: f32 = 84.0;
const SEAT_TEXT_X_OFFSET: f32 = AVATAR_SIZE + 12.0;

/// 每局随机的牌桌背景（不同场景 = 以后挂不同 buff）。
const BACKGROUNDS: [&str; 6] = [
    "bkg_Ice_field",
    "bkg_Stone_arena",
    "bkg_marsh",
    "bkg_tomb",
    "bkg_volcano",
    "bkg_wheat_field",
];
/// 每局随机的卡背。
const CARD_BACKS: [&str; 4] = ["card_back1", "card_back2", "card_back3", "card_back4"];

fn main() {
    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "RiverBorn".into(),
                resolution: (W, H).into(),
                ..default()
            }),
            ..default()
        }))
        .insert_resource(ClearColor(Color::srgb(0.08, 0.09, 0.12)))
        .insert_resource(new_session())
        .add_systems(Startup, setup_table)
        .add_systems(Update, (ai_auto_play, handle_input, refresh).chain())
        .run();
}

fn new_session() -> GameSession {
    use Class::*;
    // 1 个人类 + 3 个 AI（布局最多支持 6 个）。底牌第 4 个参数是立绘名。
    let players = vec![
        Player::new(0, "You", false, 200, [Adventurer::new(Warrior, 4, 9, "Warrior"), Adventurer::new(Cleric, 2, 6, "Cleric")]),
        Player::new(1, "AI-Gambler", true, 200, [Adventurer::new(Mage, 6, 3, "Mage"), Adventurer::new(Rogue, 5, 4, "Rogue")]),
        Player::new(2, "AI-Rookie", true, 200, [Adventurer::new(Ranger, 4, 5, "Archer"), Adventurer::new(Warrior, 3, 8, "Knight")]),
        Player::new(3, "AI-Veteran", true, 200, [Adventurer::new(Rogue, 5, 4, "Rogue"), Adventurer::new(Mage, 7, 2, "Mage")]),
    ];
    // seed 用 "RIVE" 的 ASCII，固定起手；正式版可换成随机源。
    GameSession {
        game: Game::new(players, 10, 0x5249_5645),
    }
}

/// 6 个座位的左上角坐标（前 4 个对应草图的 上/下/左/右）。
const SEATS: [(f32, f32); 6] = [
    (1000.0, 28.0),  // 0 上-中
    (1000.0, 840.0), // 1 下-中
    (24.0, 250.0),   // 2 左-中
    (1700.0, 250.0), // 3 右-中
    (24.0, 700.0),   // 4 左-下（第 5 人）
    (1700.0, 700.0), // 5 右-下（第 6 人）
];

fn setup_table(mut commands: Commands, assets: Res<AssetServer>, session: Res<GameSession>) {
    commands.spawn(Camera2d);

    // 1) 牌桌背景，铺满整窗。GlobalZIndex(-1) 明确钉在最底层
    //    （仅靠 spawn 顺序在 Bevy UI 里不保证压在底下）。
    let bg = format!("table/{}.png", pick(&BACKGROUNDS));
    commands.spawn((
        ImageNode::new(assets.load(&bg)),
        Node {
            position_type: PositionType::Absolute,
            width: Val::Percent(100.0),
            height: Val::Percent(100.0),
            ..default()
        },
        GlobalZIndex(-1),
    ));

    // 2) 选定本局卡背，存为资源，盖住的牌都用它。
    let card_back: Handle<Image> = assets.load(format!("cards/{}.png", pick(&CARD_BACKS)));
    commands.insert_resource(CardBack(card_back.clone()));

    // 3) 加载音频资源，并开始循环播放背景音乐。
    let audio_assets = AudioAssets {
        bgm: assets.load("audio/bgm.ogg"),
        check: assets.load("audio/check.ogg"),
        fold: assets.load("audio/fold.ogg"),
        raise: assets.load("audio/raise.ogg"),
    };
    commands.spawn((
        AudioPlayer::new(audio_assets.bgm.clone()),
        PlaybackSettings::LOOP,
    ));
    commands.insert_resource(audio_assets);

    // 4) 左上角状态栏（无边框纯文字）。
    commands.spawn((
        Text::new(""),
        TextFont { font_size: 22.0, ..default() },
        TextColor(Color::srgb(0.95, 0.96, 0.98)),
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(12.0),
            left: Val::Px(16.0),
            max_width: Val::Px(360.0),
            ..default()
        },
        Slot::Status,
    ));

    // 5) 公共池：5 张横排（初始卡背）。
    for i in 0..5 {
        spawn_card(
            &mut commands,
            COMMUNITY_POOL_X + i as f32 * (COMMUNITY_CARD_W + COMMUNITY_CARD_GAP),
            COMMUNITY_POOL_Y,
            COMMUNITY_CARD_W,
            COMMUNITY_CARD_H,
            COL_GOLD,
            card_back.clone(),
            Slot::Community(i),
        );
    }
    spawn_label(
        &mut commands,
        COMMUNITY_POOL_X,
        COMMUNITY_POOL_Y - 28.0,
        "Community Pool",
    );

    // 6) 地牢/Boss 池：3 个节点（初始卡背）。
    for i in 0..3 {
        spawn_card(
            &mut commands,
            DUNGEON_POOL_X + i as f32 * (DUNGEON_CARD_W + DUNGEON_CARD_GAP),
            DUNGEON_POOL_Y,
            DUNGEON_CARD_W,
            DUNGEON_CARD_H,
            COL_RED,
            card_back.clone(),
            Slot::Dungeon(i),
        );
    }
    spawn_label(
        &mut commands,
        DUNGEON_POOL_X,
        DUNGEON_POOL_Y - 28.0,
        "Dungeon / Boss",
    );

    // 7) 座位：头像 + 名字 + 两张底牌位。
    for (seat, (x, y)) in SEATS.iter().enumerate() {
        if let Some(p) = session.game.players.get(seat) {
            spawn_avatar(&mut commands, *x, *y, assets.load(avatar_path(p)));
        }
        spawn_seat_name(&mut commands, x + SEAT_TEXT_X_OFFSET, *y, seat);
        spawn_card(
            &mut commands,
            *x,
            y + HOLE_CARD_TOP_OFFSET,
            HOLE_CARD_W,
            HOLE_CARD_H,
            COL_HOLE,
            card_back.clone(),
            Slot::Hole { seat, idx: 0 },
        );
        spawn_card(
            &mut commands,
            x + HOLE_CARD_W + HOLE_CARD_GAP,
            y + HOLE_CARD_TOP_OFFSET,
            HOLE_CARD_W,
            HOLE_CARD_H,
            COL_HOLE,
            card_back.clone(),
            Slot::Hole { seat, idx: 1 },
        );
    }
}

/// 一张牌位：带边框的图片（内容由 refresh 切换成卡背或牌面）。
fn spawn_card(
    commands: &mut Commands,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    border: Color,
    img: Handle<Image>,
    slot: Slot,
) {
    commands.spawn((
        ImageNode::new(img),
        Node {
            position_type: PositionType::Absolute,
            left: Val::Px(x),
            top: Val::Px(y),
            width: Val::Px(w),
            height: Val::Px(h),
            border: UiRect::all(Val::Px(2.0)),
            ..default()
        },
        BorderColor(border),
        slot,
    ));
}

/// 座位头像（静态，不刷新）。
fn spawn_avatar(commands: &mut Commands, x: f32, y: f32, img: Handle<Image>) {
    commands.spawn((
        ImageNode::new(img),
        Node {
            position_type: PositionType::Absolute,
            left: Val::Px(x),
            top: Val::Px(y),
            width: Val::Px(AVATAR_SIZE),
            height: Val::Px(AVATAR_SIZE),
            ..default()
        },
    ));
}

/// 座位上的玩家名（可刷新、可高亮，无边框）。
fn spawn_seat_name(commands: &mut Commands, x: f32, y: f32, seat: usize) {
    commands.spawn((
        Text::new(""),
        TextFont { font_size: 17.0, ..default() },
        TextColor(Color::srgb(0.95, 0.96, 0.98)),
        Node {
            position_type: PositionType::Absolute,
            left: Val::Px(x),
            top: Val::Px(y),
            ..default()
        },
        Slot::SeatName(seat),
    ));
}

/// 静态文字标签（池子标题）。
fn spawn_label(commands: &mut Commands, x: f32, y: f32, text: &str) {
    commands.spawn((
        Text::new(text.to_string()),
        TextFont { font_size: 16.0, ..default() },
        TextColor(Color::srgb(0.75, 0.77, 0.82)),
        Node {
            position_type: PositionType::Absolute,
            left: Val::Px(x),
            top: Val::Px(y),
            ..default()
        },
    ));
}

/// 头像文件：人类用 player.png，AI 用玩家名（如 AI-Gambler.png）。
fn avatar_path(p: &Player) -> String {
    if p.is_ai {
        format!("avatars/{}.png", p.name)
    } else {
        "avatars/player.png".to_string()
    }
}

/// 用进程启动时刻做种子，从列表里挑一个（每次运行 = 每局，结果不同）。
fn pick(list: &[&'static str]) -> &'static str {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0) as usize;
    list[nanos % list.len()]
}

/// AI 玩家自动行动，用计时器拉开节奏，方便人类玩家旁观。
fn ai_auto_play(
    time: Res<Time>,
    mut delay: Local<f32>,
    mut session: ResMut<GameSession>,
    mut commands: Commands,
    audio: Res<AudioAssets>,
) {
    let g = &mut session.game;
    if g.phase == Phase::Showdown || g.phase == Phase::Done {
        return;
    }
    let idx = g.to_act;
    if !g.players[idx].is_ai {
        *delay = 0.0;
        return;
    }
    *delay += time.delta_secs();
    if *delay < 0.7 {
        return;
    }
    *delay = 0.0;
    let action = game_core::ai::decide(g, idx);
    if g.apply(action).is_ok() {
        play_action_sound(&mut commands, &audio, action);
    }
}

/// 键盘输入：Q=Check, W=Call, E=Raise+20, R=Fold。仅在轮到人类时生效。
fn handle_input(
    keys: Res<ButtonInput<KeyCode>>,
    mut session: ResMut<GameSession>,
    mut commands: Commands,
    audio: Res<AudioAssets>,
) {
    let g = &mut session.game;
    if g.phase == Phase::Showdown || g.phase == Phase::Done {
        return;
    }
    if g.players[g.to_act].is_ai {
        return;
    }
    let action = if keys.just_pressed(KeyCode::KeyQ) {
        Some(Action::Check)
    } else if keys.just_pressed(KeyCode::KeyW) {
        Some(Action::Call)
    } else if keys.just_pressed(KeyCode::KeyE) {
        Some(Action::Raise { to: g.current_bet + 20 })
    } else if keys.just_pressed(KeyCode::KeyR) {
        Some(Action::Fold)
    } else {
        None
    };
    if let Some(a) = action {
        if g.apply(a).is_ok() {
            play_action_sound(&mut commands, &audio, a);
        }
    }
}

fn play_action_sound(commands: &mut Commands, audio: &AudioAssets, action: Action) {
    let sound = match action {
        Action::Check | Action::Call => audio.check.clone(),
        Action::Fold => audio.fold.clone(),
        Action::Raise { .. } | Action::AllIn => audio.raise.clone(),
    };
    commands.spawn(AudioPlayer::new(sound));
}

/// 统一刷新所有槽位（文字槽改字、图片槽换图/换边框色）。
fn refresh(
    session: Res<GameSession>,
    back: Res<CardBack>,
    assets: Res<AssetServer>,
    mut q: Query<(Option<&mut Text>, Option<&mut ImageNode>, Option<&mut BorderColor>, &Slot)>,
) {
    let g = &session.game;
    for (text, image, border, slot) in &mut q {
        match slot {
            Slot::Status => set_text(text, status_text(g)),
            Slot::SeatName(seat) => set_text(text, seat_name_text(g, *seat)),
            Slot::Community(i) => {
                let (handle, col) = match g.community.get(*i) {
                    Some(c) => (assets.load(format!("cards/community/{}.png", c.art())), COL_GOLD),
                    None => (back.0.clone(), COL_DIM),
                };
                set_image(image, handle);
                set_border(border, col);
            }
            Slot::Dungeon(i) => {
                let (handle, col) = match g.dungeon.get(*i) {
                    Some(m) => (assets.load(format!("cards/dungeon/{}.png", m.art)), COL_RED),
                    None => (back.0.clone(), COL_DIM),
                };
                set_image(image, handle);
                set_border(border, col);
            }
            Slot::Hole { seat, idx } => {
                let (handle, col) = match g.players.get(*seat) {
                    None => (back.0.clone(), COL_DIM),
                    Some(p) => {
                        // 隐藏信息：只亮出人类自己的底牌，或摊牌阶段全亮。
                        let reveal = !p.is_ai || g.phase == Phase::Showdown;
                        let h = if reveal {
                            assets.load(format!("cards/community/{}.png", p.hole[*idx].art))
                        } else {
                            back.0.clone()
                        };
                        let c = if *seat == g.to_act && g.phase != Phase::Showdown { COL_ACTIVE } else { COL_HOLE };
                        (h, c)
                    }
                };
                set_image(image, handle);
                set_border(border, col);
            }
        }
    }
}

fn set_text(slot: Option<Mut<Text>>, s: String) {
    if let Some(mut t) = slot {
        t.0 = s;
    }
}

fn set_image(slot: Option<Mut<ImageNode>>, handle: Handle<Image>) {
    if let Some(mut img) = slot {
        img.image = handle;
    }
}

fn set_border(slot: Option<Mut<BorderColor>>, col: Color) {
    if let Some(mut b) = slot {
        b.0 = col;
    }
}

fn seat_name_text(g: &Game, seat: usize) -> String {
    match g.players.get(seat) {
        Some(p) => {
            let here = if seat == g.to_act && g.phase != Phase::Showdown { " *" } else { "" };
            format!(
                "{}{}\nChips {}\nin {} | {:?}",
                p.name, here, p.chips, p.committed, p.status
            )
        }
        None => "(empty seat)".into(),
    }
}

fn status_text(g: &Game) -> String {
    let mut s = format!("RiverBorn\nPhase: {:?}\nPot: {}   Bet: {}\n", g.phase, g.pot, g.current_bet);
    if g.phase == Phase::Showdown {
        let res = g.settle();
        s.push_str(&format!("\n=== SHOWDOWN ===\nWinner: {:?}\n", res.winner));
        for r in &res.results {
            s.push_str(&format!("P{} cleared:{} hp:{}\n", r.id.0, r.cleared, r.remaining_health));
        }
    } else {
        let actor = g.players.get(g.to_act).map(|p| p.name.as_str()).unwrap_or("-");
        s.push_str(&format!("To act: {actor}\n[Q]Check [W]Call\n[E]Raise [R]Fold"));
    }
    s
}
