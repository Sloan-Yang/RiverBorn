//! RiverBorn —— Bevy 表现层。
//!
//! 顶层是状态机 [`AppState`]：主菜单 → 单人 / 多人 / 设置。
//! - 主菜单：标题 + 三个按钮，循环 Main_menu_music。
//! - 单人：现有牌桌玩法，循环 Iron_Stakes，翻牌播 shuffle_card。
//! - 多人 / 设置：占位页，Esc 返回菜单。
//!
//! 牌桌布局（常量 + setup 里各 spawn 的坐标尺寸）是手动调好的，本层只在
//! 进入单人时按原坐标摆放、离开时整体清理；规则全在 game_core。

use bevy::audio::{AudioPlayer, AudioSource, PlaybackSettings};
use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use bevy::winit::WinitWindows;
use game_core::{Action, Adventurer, Class, Game, Phase, Player};
use winit::window::Icon;

// ============================ 状态 ============================

#[derive(States, Default, Debug, Clone, PartialEq, Eq, Hash)]
enum AppState {
    #[default]
    MainMenu,
    SinglePlayer,
    Multiplayer,
    Settings,
}

/// 单人游戏内的暂停子状态（只在 SinglePlayer 下存在）。
/// Running 时游戏逻辑正常跑；Paused/Settings 时逻辑停（即暂停），叠加菜单 overlay。
#[derive(SubStates, Default, Debug, Clone, PartialEq, Eq, Hash)]
#[source(AppState = AppState::SinglePlayer)]
enum PauseState {
    #[default]
    Running,
    Paused,
    Settings,
}

// ============================ 资源 ============================

/// 把整局游戏作为一个 Bevy 资源持有。
#[derive(Resource)]
struct GameSession {
    game: Game,
}

/// 全局音频句柄。BGM 按状态切换，动作音效在合法行动后触发。
#[derive(Resource, Clone)]
struct AudioAssets {
    game_bgm: Handle<AudioSource>, // 单人局：Iron_Stakes.mp3
    shuffle: Handle<AudioSource>,  // 发牌音效：shuffle_card.mp3
}

/// 本局选中的卡背（盖住的牌都用它）。
#[derive(Resource)]
struct CardBack(Handle<Image>);

/// 已翻开的牌总数，用来检测「又翻牌了」以触发发牌音效。
#[derive(Resource)]
struct RevealCount(usize);

// ============================ 标记 ============================

/// 每个会被刷新的位置，用这个枚举标明它代表牌桌上的哪个槽。
#[derive(Component)]
enum Slot {
    Status,                           // 左上角状态栏（文字）
    SeatName(usize),                  // 第 i 个座位的玩家名（文字）
    Community(usize),                 // 公共池第 i 张（图片）
    Dungeon(usize),                   // 地牢/Boss 池第 i 个节点（图片）
    Hole { seat: usize, idx: usize }, // 第 i 个座位的第 idx 张底牌（图片）
}

/// 主菜单按钮。
#[derive(Component)]
enum MenuButton {
    SinglePlayer,
    Multiplayer,
    Settings,
    Exit,
}

/// 暂停菜单按钮。
#[derive(Component)]
enum PauseButton {
    Resume,
    Settings,
    MainMenu,
}

/// 暂停层 overlay 的根标记，便于单独清理（不波及牌桌）。
#[derive(Component)]
struct PauseUi;

// 边框配色
const COL_DIM: Color = Color::srgb(0.30, 0.32, 0.38);    // 未翻开/空位
const COL_GOLD: Color = Color::srgb(0.85, 0.72, 0.35);   // 公共池
const COL_RED: Color = Color::srgb(0.85, 0.35, 0.35);    // 地牢/Boss
const COL_HOLE: Color = Color::srgb(0.55, 0.58, 0.65);   // 底牌
const COL_ACTIVE: Color = Color::srgb(0.40, 0.85, 0.45); // 当前行动高亮
const BTN_NORMAL: Color = Color::srgb(0.16, 0.18, 0.26); // 菜单按钮常态
const BTN_HOVER: Color = Color::srgb(0.28, 0.32, 0.44);  // 菜单按钮悬停

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
        .init_state::<AppState>()
        .add_sub_state::<PauseState>()
        .add_systems(Startup, (setup, set_window_icon))
        // 主菜单
        .add_systems(OnEnter(AppState::MainMenu), enter_main_menu)
        .add_systems(Update, menu_buttons.run_if(in_state(AppState::MainMenu)))
        // 单人：游戏逻辑只在「未暂停」时跑
        .add_systems(OnEnter(AppState::SinglePlayer), enter_single_player)
        .add_systems(
            Update,
            (ai_auto_play, handle_input, deal_sfx)
                .chain()
                .run_if(in_state(PauseState::Running)),
        )
        // 画面刷新单人全程都跑（暂停时也要显示牌桌）
        .add_systems(Update, refresh.run_if(in_state(AppState::SinglePlayer)))
        // 单人内的暂停切换 + 暂停菜单
        .add_systems(Update, pause_input.run_if(in_state(AppState::SinglePlayer)))
        .add_systems(Update, pause_buttons.run_if(in_state(PauseState::Paused)))
        .add_systems(OnEnter(PauseState::Paused), enter_pause_menu)
        .add_systems(OnEnter(PauseState::Settings), enter_pause_settings)
        .add_systems(OnExit(PauseState::Paused), cleanup_pause)
        .add_systems(OnExit(PauseState::Settings), cleanup_pause)
        // 多人 / 设置（占位）
        .add_systems(OnEnter(AppState::Multiplayer), enter_multiplayer)
        .add_systems(OnEnter(AppState::Settings), enter_settings)
        // 多人/设置顶层页面按 Esc 返回主菜单（单人的 Esc 交给 pause_input）
        .add_systems(
            Update,
            back_to_menu.run_if(|s: Res<State<AppState>>| {
                matches!(*s.get(), AppState::Multiplayer | AppState::Settings)
            }),
        )
        // 每个状态退出时清理它生成的 UI 与音频
        .add_systems(OnExit(AppState::MainMenu), cleanup)
        .add_systems(OnExit(AppState::SinglePlayer), cleanup)
        .add_systems(OnExit(AppState::Multiplayer), cleanup)
        .add_systems(OnExit(AppState::Settings), cleanup)
        .run();
}

/// Startup：全局相机 + 预加载所有音频句柄。
fn setup(mut commands: Commands, assets: Res<AssetServer>) {
    commands.spawn(Camera2d);
    commands.insert_resource(AudioAssets {
        game_bgm: assets.load("audio/Iron_Stakes.mp3"),
        shuffle: assets.load("audio/shuffle_card.mp3"),
    });
}

// ============================ 主菜单 ============================

fn enter_main_menu(mut commands: Commands, assets: Res<AssetServer>) {
    // 菜单背景，铺满整窗、钉在最底层。
    commands.spawn((
        ImageNode::new(assets.load("main_menu.png")),
        Node {
            position_type: PositionType::Absolute,
            width: Val::Percent(100.0),
            height: Val::Percent(100.0),
            ..default()
        },
        GlobalZIndex(-1),
    ));

    // 菜单 BGM（循环）。离开菜单时由 cleanup 停掉。
    // 注意：初始状态的 OnEnter 比 Startup 还早，这里不能依赖预载的 AudioAssets，
    // 直接用 AssetServer 加载。
    commands.spawn((
        AudioPlayer::new(assets.load("audio/Main_menu_music.mp3")),
        PlaybackSettings::LOOP,
    ));

    commands
        .spawn(Node {
            width: Val::Percent(100.0),
            height: Val::Percent(100.0),
            flex_direction: FlexDirection::Column,
            justify_content: JustifyContent::Center,
            align_items: AlignItems::Center,
            row_gap: Val::Px(24.0),
            ..default()
        })
        .with_children(|root| {
            // 游戏名称图（1301×515，按比例缩到 650×257）。
            root.spawn((
                ImageNode::new(assets.load("game_name.png")),
                Node {
                    width: Val::Px(650.0),
                    height: Val::Px(257.0),
                    margin: UiRect::bottom(Val::Px(32.0)),
                    ..default()
                },
            ));
            menu_button(root, MenuButton::SinglePlayer, "Single Player");
            menu_button(root, MenuButton::Multiplayer, "Multiplayer");
            menu_button(root, MenuButton::Settings, "Settings");
            menu_button(root, MenuButton::Exit, "Exit");
        });
}

fn menu_button(parent: &mut ChildBuilder, action: MenuButton, label: &str) {
    parent
        .spawn((
            Button,
            Node {
                width: Val::Px(360.0),
                height: Val::Px(72.0),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                border: UiRect::all(Val::Px(2.0)),
                ..default()
            },
            BorderColor(Color::srgb(0.45, 0.48, 0.58)),
            BackgroundColor(BTN_NORMAL),
            action,
        ))
        .with_children(|b| {
            b.spawn((
                Text::new(label),
                TextFont { font_size: 30.0, ..default() },
                TextColor(Color::srgb(0.92, 0.94, 0.97)),
            ));
        });
}

fn menu_buttons(
    mut next: ResMut<NextState<AppState>>,
    mut exit: EventWriter<AppExit>,
    mut q: Query<(&Interaction, &MenuButton, &mut BackgroundColor), Changed<Interaction>>,
) {
    for (interaction, action, mut bg) in &mut q {
        match interaction {
            Interaction::Pressed => match action {
                MenuButton::SinglePlayer => next.set(AppState::SinglePlayer),
                MenuButton::Multiplayer => next.set(AppState::Multiplayer),
                MenuButton::Settings => next.set(AppState::Settings),
                MenuButton::Exit => {
                    exit.send(AppExit::Success);
                }
            },
            Interaction::Hovered => bg.0 = BTN_HOVER,
            Interaction::None => bg.0 = BTN_NORMAL,
        }
    }
}

// ============================ 单人游戏 ============================

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

fn enter_single_player(mut commands: Commands, assets: Res<AssetServer>, audio: Res<AudioAssets>) {
    let session = new_session();

    // 游戏内 BGM（循环）+ 入局发牌音效（一次性，播完自动 despawn）。
    commands.spawn((AudioPlayer::new(audio.game_bgm.clone()), PlaybackSettings::LOOP));
    commands.spawn((AudioPlayer::new(audio.shuffle.clone()), PlaybackSettings::DESPAWN));

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

    // 3) 左上角状态栏（无边框纯文字）。
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

    // 4) 公共池：5 张横排（初始卡背）。
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

    // 5) 地牢/Boss 池：3 个节点（初始卡背）。
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

    // 6) 座位：头像 + 名字 + 两张底牌位。
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

    commands.insert_resource(RevealCount(0));
    commands.insert_resource(session);
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

/// 从列表里随机挑一个（每次运行 = 每局，结果不同）。
///
/// 注意：不能直接用 `纳秒 % len`——Windows 系统时间精度是 100ns，纳秒恒为 100 的
/// 倍数，`% 6` 只会落在 {0,2,4}，导致一半场景永远抽不中。这里先用 splitmix64
/// 把时间种子打散，再取模。混入数组地址，让相邻的两次调用（背景/卡背）互相独立。
fn pick(list: &[&'static str]) -> &'static str {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0) as u64;
    let seed = nanos ^ (list.as_ptr() as u64);
    let r = game_core::rng::Rng::new(seed).next_u64() as usize;
    list[r % list.len()]
}

/// 翻牌时（已揭示牌数增加）播放一次发牌音效。
fn deal_sfx(
    mut commands: Commands,
    audio: Res<AudioAssets>,
    session: Res<GameSession>,
    mut reveal: ResMut<RevealCount>,
) {
    let revealed = session.game.community.len() + session.game.dungeon.len();
    if revealed > reveal.0 {
        commands.spawn((AudioPlayer::new(audio.shuffle.clone()), PlaybackSettings::DESPAWN));
    }
    reveal.0 = revealed;
}

/// AI 玩家自动行动，用计时器拉开节奏，方便人类玩家旁观。
fn ai_auto_play(time: Res<Time>, mut delay: Local<f32>, mut session: ResMut<GameSession>) {
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
    let _ = g.apply(action);
}

/// 键盘输入：Q=Check, W=Call, E=Raise+20, R=Fold。仅在轮到人类时生效。
fn handle_input(keys: Res<ButtonInput<KeyCode>>, mut session: ResMut<GameSession>) {
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
        let _ = g.apply(a);
    }
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
        s.push_str(&format!("To act: {actor}\n[Q]Check [W]Call\n[E]Raise [R]Fold\n[Esc] Menu"));
    }
    s
}

// ============================ 多人 / 设置（占位）============================

fn enter_multiplayer(mut commands: Commands) {
    let plan = "\
联机部分尚未实现，先放规划：

· 架构：服务器权威。发牌/下注/战斗结算都在服务器用 game_core 跑，\n  客户端只发动作、收状态。
· 隐藏信息：底牌只下发给本人，摊牌前对手底牌不出现在该客户端。
· 同步：bevy_replicon 或 lightyear 做状态复制；game_core 的确定性\n  + 固定 seed 便于对齐。
· 大厅：创建/加入房间，2–6 人入座后开局。

（game_core 已与引擎解耦，这些都能直接复用现有规则逻辑。）";
    spawn_info_screen(&mut commands, "Multiplayer — Coming Soon", plan);
}

fn enter_settings(mut commands: Commands) {
    spawn_info_screen(&mut commands, "Settings", "设置项待定（音量、分辨率、按键等）。");
}

/// 居中的信息页：大标题 + 正文 + 返回提示。
fn spawn_info_screen(commands: &mut Commands, title: &str, body: &str) {
    commands
        .spawn(Node {
            width: Val::Percent(100.0),
            height: Val::Percent(100.0),
            flex_direction: FlexDirection::Column,
            justify_content: JustifyContent::Center,
            align_items: AlignItems::Center,
            row_gap: Val::Px(20.0),
            ..default()
        })
        .with_children(|root| {
            root.spawn((
                Text::new(title),
                TextFont { font_size: 48.0, ..default() },
                TextColor(Color::srgb(0.95, 0.85, 0.55)),
            ));
            root.spawn((
                Text::new(body),
                TextFont { font_size: 22.0, ..default() },
                TextColor(Color::srgb(0.82, 0.84, 0.88)),
                Node { max_width: Val::Px(1000.0), ..default() },
            ));
            root.spawn((
                Text::new("Press [Esc] to go back"),
                TextFont { font_size: 20.0, ..default() },
                TextColor(Color::srgb(0.55, 0.58, 0.65)),
            ));
        });
}

/// 任意非主菜单状态下，按 Esc 回主菜单。
fn back_to_menu(keys: Res<ButtonInput<KeyCode>>, mut next: ResMut<NextState<AppState>>) {
    if keys.just_pressed(KeyCode::Escape) {
        next.set(AppState::MainMenu);
    }
}

/// 离开任一状态时，清掉它生成的所有 UI（根节点递归）与音频实体。
/// 只删根节点，children 由 despawn_recursive 一并带走，避免重复删除告警。
/// 全局相机不是 UI 节点、不是音频，不受影响。
fn cleanup(
    mut commands: Commands,
    roots: Query<Entity, (With<Node>, Without<Parent>)>,
    audio: Query<Entity, With<AudioPlayer>>,
) {
    for e in &roots {
        commands.entity(e).despawn_recursive();
    }
    for e in &audio {
        commands.entity(e).despawn_recursive();
    }
}

// ============================ 暂停菜单（单人内）============================

/// 单人游戏中按 Esc 在 运行 / 暂停 / 设置 之间切换。
fn pause_input(
    keys: Res<ButtonInput<KeyCode>>,
    cur: Res<State<PauseState>>,
    mut next: ResMut<NextState<PauseState>>,
) {
    if !keys.just_pressed(KeyCode::Escape) {
        return;
    }
    next.set(match cur.get() {
        PauseState::Running => PauseState::Paused,  // 暂停
        PauseState::Paused => PauseState::Running,  // 继续
        PauseState::Settings => PauseState::Paused, // 从设置回暂停菜单
    });
}

/// 弹出暂停菜单 overlay：半透明遮罩 + 标题 + 三个按钮。
fn enter_pause_menu(mut commands: Commands) {
    commands
        .spawn((
            Node {
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                row_gap: Val::Px(20.0),
                ..default()
            },
            BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.72)),
            GlobalZIndex(100), // 盖在牌桌之上
            PauseUi,
        ))
        .with_children(|root| {
            root.spawn((
                Text::new("Paused"),
                TextFont { font_size: 60.0, ..default() },
                TextColor(Color::srgb(0.95, 0.85, 0.55)),
                Node { margin: UiRect::bottom(Val::Px(24.0)), ..default() },
            ));
            pause_button(root, PauseButton::Resume, "Resume");
            pause_button(root, PauseButton::Settings, "Settings");
            pause_button(root, PauseButton::MainMenu, "Main Menu");
        });
}

/// 暂停内的设置 overlay（占位）。
fn enter_pause_settings(mut commands: Commands) {
    commands
        .spawn((
            Node {
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                row_gap: Val::Px(20.0),
                ..default()
            },
            BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.85)),
            GlobalZIndex(100),
            PauseUi,
        ))
        .with_children(|root| {
            root.spawn((
                Text::new("Settings"),
                TextFont { font_size: 48.0, ..default() },
                TextColor(Color::srgb(0.95, 0.85, 0.55)),
            ));
            root.spawn((
                Text::new("设置项待定（音量、分辨率、按键等）。"),
                TextFont { font_size: 22.0, ..default() },
                TextColor(Color::srgb(0.82, 0.84, 0.88)),
            ));
            root.spawn((
                Text::new("Press [Esc] to go back"),
                TextFont { font_size: 20.0, ..default() },
                TextColor(Color::srgb(0.55, 0.58, 0.65)),
            ));
        });
}

fn pause_button(parent: &mut ChildBuilder, action: PauseButton, label: &str) {
    parent
        .spawn((
            Button,
            Node {
                width: Val::Px(320.0),
                height: Val::Px(64.0),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                border: UiRect::all(Val::Px(2.0)),
                ..default()
            },
            BorderColor(Color::srgb(0.45, 0.48, 0.58)),
            BackgroundColor(BTN_NORMAL),
            action,
        ))
        .with_children(|b| {
            b.spawn((
                Text::new(label),
                TextFont { font_size: 26.0, ..default() },
                TextColor(Color::srgb(0.92, 0.94, 0.97)),
            ));
        });
}

/// 处理暂停菜单按钮。
fn pause_buttons(
    mut next_pause: ResMut<NextState<PauseState>>,
    mut next_app: ResMut<NextState<AppState>>,
    mut q: Query<(&Interaction, &PauseButton, &mut BackgroundColor), Changed<Interaction>>,
) {
    for (interaction, action, mut bg) in &mut q {
        match interaction {
            Interaction::Pressed => match action {
                PauseButton::Resume => next_pause.set(PauseState::Running),
                PauseButton::Settings => next_pause.set(PauseState::Settings),
                PauseButton::MainMenu => next_app.set(AppState::MainMenu),
            },
            Interaction::Hovered => bg.0 = BTN_HOVER,
            Interaction::None => bg.0 = BTN_NORMAL,
        }
    }
}

/// 清理暂停层 overlay（继续游戏 / 切到设置 / 离开单人时）。
fn cleanup_pause(mut commands: Commands, q: Query<Entity, With<PauseUi>>) {
    for e in &q {
        commands.entity(e).despawn_recursive();
    }
}

// ============================ 窗口图标 ============================

/// 把 assets/game_icon.png 设为窗口图标。字节编译进二进制，不依赖运行目录。
fn set_window_icon(windows: NonSend<WinitWindows>, primary: Query<Entity, With<PrimaryWindow>>) {
    let Ok(entity) = primary.get_single() else {
        return;
    };
    let Some(window) = windows.get_window(entity) else {
        return;
    };
    let bytes = include_bytes!("../assets/game_icon.png");
    let rgba = match image::load_from_memory(bytes) {
        Ok(img) => img.into_rgba8(),
        Err(_) => return,
    };
    let (w, h) = rgba.dimensions();
    if let Ok(icon) = Icon::from_rgba(rgba.into_raw(), w, h) {
        window.set_window_icon(Some(icon));
    }
}
