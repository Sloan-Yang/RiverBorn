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
use bevy::input::keyboard::{Key, KeyboardInput};
use bevy::input::ButtonState;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use bevy::winit::WinitWindows;
use bevy_renet::netcode::{ClientAuthentication, NetcodeClientPlugin, NetcodeClientTransport};
use bevy_renet::renet::{ConnectionConfig, DefaultChannel, RenetClient};
use bevy_renet::RenetClientPlugin;
use game_core::{Action, Match, Phase, SeatDef};
use riverborn_net::{ClientMsg, PlayerId, RoomInfo, RoomState, ServerMsg, DEFAULT_PORT, PROTOCOL_ID};
use std::net::{SocketAddr, UdpSocket};
use std::time::{SystemTime, UNIX_EPOCH};
use winit::window::Icon;

// ============================ 状态 ============================

#[derive(States, Default, Debug, Clone, PartialEq, Eq, Hash)]
enum AppState {
    #[default]
    MainMenu,
    SinglePlayerSetup, // 选人数(3-6)的页面
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

/// 联机内的子状态（只在 Multiplayer 下存在）：输入名字 → 大厅 → 房间。
#[derive(SubStates, Default, Debug, Clone, PartialEq, Eq, Hash)]
#[source(AppState = AppState::Multiplayer)]
enum MpState {
    #[default]
    NameEntry,
    Menu,
    Lobby,
}

// ============================ 资源 ============================

/// 把整场对局（多手循环 + 银行账本）作为一个 Bevy 资源持有。
#[derive(Resource)]
struct GameSession {
    mtch: Match,
}

/// 全局音频句柄。BGM 按状态切换，动作音效在合法行动后触发。
#[derive(Resource, Clone)]
struct AudioAssets {
    game_bgm: Handle<AudioSource>, // 单人局：Iron_Stakes.mp3
    shuffle: Handle<AudioSource>,  // 发牌音效：shuffle_card.mp3
}

/// 全局 UI 字体（含中文字形；Bevy 默认字体不含中文会乱码）。
#[derive(Resource)]
struct UiFont(Handle<Font>);

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

/// 单机对局的玩家人数（含人类），在选人数页面设定。
#[derive(Resource)]
struct PlayerCount(usize);

/// 联机会话状态（只在 Multiplayer 期间存在）。
#[derive(Resource, Default)]
struct Net {
    my_id: Option<PlayerId>,
    name: String,         // 已确认的名字
    greeted: bool,        // 是否已向服务器发过 Hello
    name_input: String,   // 名字输入框内容
    code_input: String,   // 房间号输入框内容
    rooms: Vec<RoomInfo>, // 搜索到的房间
    room: Option<RoomState>, // 当前所在房间
    status: String,       // 提示 / 错误
}

/// 联机各子屏的根标记（OnExit 清理）。
#[derive(Component)]
struct MpUi;

/// 联机界面按钮。
#[derive(Component)]
enum MpButton {
    Confirm,       // 名字确定
    Create(usize), // 创建指定人数的房间
    Refresh,       // 刷新房间列表
    Join,          // 按房间号加入
    Start,         // 房主开始
    Leave,         // 离开房间
}

/// 联机界面里随 Net 刷新的文字。
#[derive(Component)]
enum MpText {
    NameInput,
    CodeInput,
    RoomList,
    Lobby,
    Status,
}

/// 主菜单按钮。
#[derive(Component)]
enum MenuButton {
    SinglePlayer,
    Multiplayer,
    Settings,
    Exit,
}

/// 选人数页面的根标记。
#[derive(Component)]
struct SetupUi;

/// 选人数按钮（携带人数 3~6）。
#[derive(Component)]
struct SetupButton(usize);

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

/// 摊牌结算面板（含「下一手」按钮）的根标记。
#[derive(Component)]
struct ResultUi;

/// 记账面板（Tab 打开）的根标记。
#[derive(Component)]
struct LedgerUi;

/// 记账面板里随时刷新的文本节点。
#[derive(Component)]
struct LedgerText;

/// 「下一手」按钮。
#[derive(Component)]
struct NextHandButton;

/// 「借一锅」按钮（人类筹码不足时显示）。
#[derive(Component)]
struct BorrowButton;

/// 牌桌背景节点（每手按场景换图）。
#[derive(Component)]
struct SceneBackground;

/// 叠在某个牌槽上的数值文字（power/health 或 threat/health）。
/// 复用 [`Slot`] 标明它对应哪个槽；refresh 里同一 arm 顺带刷新它的文字。
#[derive(Component)]
struct ValueLabel;

/// 摊牌时人类点选的公共牌下标（最多 3 张）。该资源存在 = 正在点选阶段。
#[derive(Resource, Default)]
struct Selection {
    picks: Vec<usize>,
}

/// 可点选的公共牌（携带它在公共池里的下标）。
#[derive(Component)]
struct Selectable(usize);

/// 点选阶段的提示 + 确认 UI 根标记。
#[derive(Component)]
struct SelectUi;

/// 点选提示里的「已选 X/3」文字。
#[derive(Component)]
struct SelectCountText;

/// 「确认组队」按钮。
#[derive(Component)]
struct ConfirmButton;

// 边框配色
const COL_DIM: Color = Color::srgb(0.30, 0.32, 0.38);    // 未翻开/空位
const COL_GOLD: Color = Color::srgb(0.85, 0.72, 0.35);   // 公共池
const COL_RED: Color = Color::srgb(0.85, 0.35, 0.35);    // 地牢/Boss
const COL_HOLE: Color = Color::srgb(0.55, 0.58, 0.65);   // 底牌
const COL_ACTIVE: Color = Color::srgb(0.40, 0.85, 0.45); // 当前行动高亮
const COL_SELECT: Color = Color::srgb(0.30, 0.85, 0.95);  // 摊牌点选时被选中的公共牌
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

/// 牌桌背景现由 game_core 的 [`Scene`] 决定（每手随机 + 挂 buff），见 sync_scene_bg。
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
        .add_plugins((RenetClientPlugin, NetcodeClientPlugin))
        .init_state::<AppState>()
        .add_sub_state::<PauseState>()
        .add_sub_state::<MpState>()
        .add_systems(Startup, (setup, set_window_icon))
        .add_systems(Update, apply_ui_font)
        // 主菜单
        .add_systems(OnEnter(AppState::MainMenu), enter_main_menu)
        .add_systems(Update, menu_buttons.run_if(in_state(AppState::MainMenu)))
        // 选人数页面
        .add_systems(OnEnter(AppState::SinglePlayerSetup), enter_setup_screen)
        .add_systems(Update, setup_buttons.run_if(in_state(AppState::SinglePlayerSetup)))
        .add_systems(OnExit(AppState::SinglePlayerSetup), cleanup)
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
        // 摊牌结算面板 / 借锅 / 记账 / 场景背景：单人全程
        .add_systems(
            Update,
            (
                showdown_director,
                selection_clicks,
                selection_ui_update,
                confirm_selection,
                result_buttons,
                borrow_button,
                ledger_toggle,
                update_ledger,
                sync_scene_bg,
            )
                .run_if(in_state(AppState::SinglePlayer)),
        )
        // 单人内的暂停切换 + 暂停菜单
        .add_systems(Update, pause_input.run_if(in_state(AppState::SinglePlayer)))
        .add_systems(Update, pause_buttons.run_if(in_state(PauseState::Paused)))
        .add_systems(OnEnter(PauseState::Paused), enter_pause_menu)
        .add_systems(OnEnter(PauseState::Settings), enter_pause_settings)
        .add_systems(OnExit(PauseState::Paused), cleanup_pause)
        .add_systems(OnExit(PauseState::Settings), cleanup_pause)
        // 多人联机：连接 + 子屏（输入名字 / 大厅 / 房间）
        .add_systems(OnEnter(AppState::Multiplayer), mp_connect)
        .add_systems(OnExit(AppState::Multiplayer), mp_disconnect)
        .add_systems(
            Update,
            (mp_receive, mp_send_hello, mp_buttons, mp_text_input, mp_refresh_ui)
                .run_if(in_state(AppState::Multiplayer)),
        )
        .add_systems(OnEnter(MpState::NameEntry), enter_name_entry)
        .add_systems(OnEnter(MpState::Menu), enter_mp_menu)
        .add_systems(OnEnter(MpState::Lobby), enter_lobby)
        .add_systems(OnExit(MpState::NameEntry), cleanup_mp)
        .add_systems(OnExit(MpState::Menu), cleanup_mp)
        .add_systems(OnExit(MpState::Lobby), cleanup_mp)
        // 设置（占位）
        .add_systems(OnEnter(AppState::Settings), enter_settings)
        // 多人/设置顶层页面按 Esc 返回主菜单（单人的 Esc 交给 pause_input）
        .add_systems(
            Update,
            back_to_menu.run_if(|s: Res<State<AppState>>| {
                matches!(*s.get(), AppState::Multiplayer | AppState::Settings | AppState::SinglePlayerSetup)
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
    commands.insert_resource(UiFont(assets.load("fonts/simhei.ttf")));
    commands.insert_resource(PlayerCount(4)); // 默认 4 人，选人数页面会覆盖
    commands.insert_resource(AudioAssets {
        game_bgm: assets.load("audio/Iron_Stakes.mp3"),
        shuffle: assets.load("audio/shuffle_card.mp3"),
    });
}

/// 把所有「还在用默认字体」的文字节点换成含中文的字体。
/// 用一个常驻系统统一处理，省得在每个 spawn 处都传字体句柄；
/// 一旦换过就不再匹配（font != 默认），开销极小。
fn apply_ui_font(font: Res<UiFont>, mut q: Query<&mut TextFont>) {
    for mut tf in &mut q {
        if tf.font == Handle::default() {
            tf.font = font.0.clone();
        }
    }
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
                MenuButton::SinglePlayer => next.set(AppState::SinglePlayerSetup),
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

// ============================ 选人数页面 ============================

fn enter_setup_screen(mut commands: Commands, assets: Res<AssetServer>) {
    // 沿用菜单 BGM，避免选人数时静音。
    commands.spawn((
        AudioPlayer::new(assets.load("audio/Main_menu_music.mp3")),
        PlaybackSettings::LOOP,
    ));

    commands
        .spawn((
            Node {
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                row_gap: Val::Px(28.0),
                ..default()
            },
            BackgroundColor(Color::srgb(0.08, 0.09, 0.12)),
            SetupUi,
        ))
        .with_children(|root| {
            root.spawn((
                Text::new("选择玩家人数"),
                TextFont { font_size: 48.0, ..default() },
                TextColor(Color::srgb(0.95, 0.85, 0.55)),
                Node { margin: UiRect::bottom(Val::Px(16.0)), ..default() },
            ));
            // 一排 3/4/5/6 按钮。
            root.spawn(Node {
                flex_direction: FlexDirection::Row,
                column_gap: Val::Px(24.0),
                ..default()
            })
            .with_children(|row| {
                for n in 3..=6usize {
                    row.spawn((
                        Button,
                        SetupButton(n),
                        Node {
                            width: Val::Px(110.0),
                            height: Val::Px(110.0),
                            justify_content: JustifyContent::Center,
                            align_items: AlignItems::Center,
                            border: UiRect::all(Val::Px(2.0)),
                            ..default()
                        },
                        BorderColor(Color::srgb(0.45, 0.48, 0.58)),
                        BackgroundColor(BTN_NORMAL),
                    ))
                    .with_children(|b| {
                        b.spawn((
                            Text::new(format!("{n}")),
                            TextFont { font_size: 48.0, ..default() },
                            TextColor(Color::srgb(0.92, 0.94, 0.97)),
                        ));
                    });
                }
            });
            root.spawn((
                Text::new("1 名玩家 + 其余 AI    ·    [Esc] 返回"),
                TextFont { font_size: 20.0, ..default() },
                TextColor(Color::srgb(0.6, 0.63, 0.7)),
                Node { margin: UiRect::top(Val::Px(16.0)), ..default() },
            ));
        });
}

fn setup_buttons(
    mut count: ResMut<PlayerCount>,
    mut next: ResMut<NextState<AppState>>,
    mut q: Query<(&Interaction, &SetupButton, &mut BackgroundColor), Changed<Interaction>>,
) {
    for (interaction, b, mut bg) in &mut q {
        match interaction {
            Interaction::Pressed => {
                count.0 = b.0;
                next.set(AppState::SinglePlayer);
            }
            Interaction::Hovered => bg.0 = BTN_HOVER,
            Interaction::None => bg.0 = BTN_NORMAL,
        }
    }
}

// ============================ 单人游戏 ============================

/// AI 立绘文件名（只有这 3 张图，超过则循环复用）。
const AI_AVATARS: [&str; 3] = ["AI-Gambler", "AI-Rookie", "AI-Veteran"];

fn new_session(count: usize) -> GameSession {
    let count = count.clamp(3, 6);
    // 1 个人类 + (count-1) 个 AI。每人由 Match 统一买入 1 锅。
    let mut defs = vec![SeatDef::new("You", false)];
    for k in 0..count - 1 {
        // AI 名字：前 3 个用固定名，更多则在名字后加序号。
        let name = if k < AI_AVATARS.len() {
            AI_AVATARS[k].to_string()
        } else {
            format!("{} {}", AI_AVATARS[k % AI_AVATARS.len()], k / AI_AVATARS.len() + 1)
        };
        defs.push(SeatDef::new(name, true));
    }
    // 每场对局用随机种子，保证每次牌局都不同。
    GameSession {
        mtch: Match::new(defs, random_seed()),
    }
}

/// 头像文件：座位 0 是人类(player.png)，其余 AI 按序号循环复用 3 张 AI 立绘。
fn avatar_file(seat: usize) -> String {
    if seat == 0 {
        "avatars/player.png".to_string()
    } else {
        format!("avatars/{}.png", AI_AVATARS[(seat - 1) % AI_AVATARS.len()])
    }
}

/// 按人数返回各座位的左上角坐标（数量恰好等于人数，无空座）。
/// 3：上 + 左下 + 右下；4：上下左右；5：上 + 两侧 + 左下右下；6：上下 + 四角。
fn seat_positions(count: usize) -> Vec<(f32, f32)> {
    const TOP: (f32, f32) = (1000.0, 28.0);
    const BOTTOM: (f32, f32) = (1000.0, 840.0);
    const TOP_LEFT: (f32, f32) = (24.0, 250.0);
    const TOP_RIGHT: (f32, f32) = (1700.0, 250.0);
    const MID_LEFT: (f32, f32) = (24.0, 440.0);
    const MID_RIGHT: (f32, f32) = (1700.0, 440.0);
    const BOTTOM_LEFT: (f32, f32) = (24.0, 700.0);
    const BOTTOM_RIGHT: (f32, f32) = (1700.0, 700.0);
    match count {
        3 => vec![TOP, BOTTOM_LEFT, BOTTOM_RIGHT],
        4 => vec![TOP, BOTTOM, MID_LEFT, MID_RIGHT],
        5 => vec![TOP, MID_LEFT, MID_RIGHT, BOTTOM_LEFT, BOTTOM_RIGHT],
        _ => vec![TOP, BOTTOM, TOP_LEFT, TOP_RIGHT, BOTTOM_LEFT, BOTTOM_RIGHT], // 6（也兜底）
    }
}

/// 用系统时间生成一个打散过的随机种子（time % N 在 Windows 上有偏置，先过 PRNG）。
fn random_seed() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0) as u64;
    game_core::rng::Rng::new(nanos ^ 0xA5A5_5A5A_1234_ABCD).next_u64()
}

fn enter_single_player(
    mut commands: Commands,
    assets: Res<AssetServer>,
    audio: Res<AudioAssets>,
    count: Res<PlayerCount>,
) {
    let session = new_session(count.0);

    // 游戏内 BGM（循环）+ 入局发牌音效（一次性，播完自动 despawn）。
    commands.spawn((AudioPlayer::new(audio.game_bgm.clone()), PlaybackSettings::LOOP));
    commands.spawn((AudioPlayer::new(audio.shuffle.clone()), PlaybackSettings::DESPAWN));

    // 1) 牌桌背景，铺满整窗。GlobalZIndex(-1) 明确钉在最底层
    //    （仅靠 spawn 顺序在 Bevy UI 里不保证压在底下）。背景由本手场景决定，
    //    sync_scene_bg 每手换图。
    let bg = format!("table/{}.png", session.mtch.scene.art());
    commands.spawn((
        ImageNode::new(assets.load(&bg)),
        Node {
            position_type: PositionType::Absolute,
            width: Val::Percent(100.0),
            height: Val::Percent(100.0),
            ..default()
        },
        GlobalZIndex(-1),
        SceneBackground,
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

    // 4) 公共池：5 张横排（初始卡背）。摊牌时人类可点选其中 3 张组队，
    //    所以挂上 Button/Interaction/Selectable。
    for i in 0..5 {
        let x = COMMUNITY_POOL_X + i as f32 * (COMMUNITY_CARD_W + COMMUNITY_CARD_GAP);
        let e = spawn_card(
            &mut commands,
            x,
            COMMUNITY_POOL_Y,
            COMMUNITY_CARD_W,
            COMMUNITY_CARD_H,
            COL_GOLD,
            card_back.clone(),
            Slot::Community(i),
        );
        commands.entity(e).insert((Button, Interaction::default(), Selectable(i)));
        spawn_value_label(&mut commands, x, COMMUNITY_POOL_Y, COMMUNITY_CARD_W, COMMUNITY_CARD_H, Slot::Community(i));
    }
    spawn_label(
        &mut commands,
        COMMUNITY_POOL_X,
        COMMUNITY_POOL_Y - 28.0,
        "Community Pool",
    );

    // 5) 地牢/Boss 池：3 个节点（初始卡背）。
    for i in 0..3 {
        let x = DUNGEON_POOL_X + i as f32 * (DUNGEON_CARD_W + DUNGEON_CARD_GAP);
        spawn_card(
            &mut commands,
            x,
            DUNGEON_POOL_Y,
            DUNGEON_CARD_W,
            DUNGEON_CARD_H,
            COL_RED,
            card_back.clone(),
            Slot::Dungeon(i),
        );
        spawn_value_label(&mut commands, x, DUNGEON_POOL_Y, DUNGEON_CARD_W, DUNGEON_CARD_H, Slot::Dungeon(i));
    }
    spawn_label(
        &mut commands,
        DUNGEON_POOL_X,
        DUNGEON_POOL_Y - 28.0,
        "Dungeon / Boss",
    );

    // 6) 座位：按人数取布局，逐个真实玩家摆头像 + 名字 + 两张底牌位（无空座）。
    let positions = seat_positions(session.mtch.seats.len());
    for (seat, (x, y)) in positions.iter().enumerate() {
        spawn_avatar(&mut commands, *x, *y, assets.load(avatar_file(seat)));
        spawn_seat_name(&mut commands, x + SEAT_TEXT_X_OFFSET, *y, seat);
        let x0 = *x;
        let x1 = x + HOLE_CARD_W + HOLE_CARD_GAP;
        let cy = y + HOLE_CARD_TOP_OFFSET;
        spawn_card(&mut commands, x0, cy, HOLE_CARD_W, HOLE_CARD_H, COL_HOLE, card_back.clone(), Slot::Hole { seat, idx: 0 });
        spawn_value_label(&mut commands, x0, cy, HOLE_CARD_W, HOLE_CARD_H, Slot::Hole { seat, idx: 0 });
        spawn_card(&mut commands, x1, cy, HOLE_CARD_W, HOLE_CARD_H, COL_HOLE, card_back.clone(), Slot::Hole { seat, idx: 1 });
        spawn_value_label(&mut commands, x1, cy, HOLE_CARD_W, HOLE_CARD_H, Slot::Hole { seat, idx: 1 });
    }

    // 7) 「借一锅」按钮：默认隐藏，borrow_button 系统按人类筹码情况显隐。
    //    放在左下角空白处，不与牌桌布局冲突；要挪位改这里的 left/top 即可。
    commands
        .spawn((
            Button,
            BorrowButton,
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(24.0),
                top: Val::Px(980.0),
                width: Val::Px(240.0),
                height: Val::Px(56.0),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                border: UiRect::all(Val::Px(2.0)),
                display: Display::None,
                ..default()
            },
            BorderColor(Color::srgb(0.85, 0.72, 0.35)),
            BackgroundColor(Color::srgb(0.20, 0.16, 0.10)),
            GlobalZIndex(40),
        ))
        .with_children(|b| {
            b.spawn((
                Text::new("借一锅 (+1000)"),
                TextFont { font_size: 24.0, ..default() },
                TextColor(Color::srgb(0.95, 0.85, 0.55)),
            ));
        });

    // 清掉可能残留的点选状态（上次中途离开单人留下的）。
    commands.remove_resource::<Selection>();
    commands.insert_resource(RevealCount(0));
    commands.insert_resource(session);
}

/// 叠在牌槽底部的数值文字（refresh 据 Slot 填内容；盖着的牌为空）。
fn spawn_value_label(commands: &mut Commands, x: f32, y: f32, w: f32, h: f32, slot: Slot) {
    commands.spawn((
        Text::new(""),
        TextFont { font_size: 18.0, ..default() },
        TextColor(Color::srgb(0.98, 0.98, 0.80)),
        TextLayout::new_with_justify(JustifyText::Center),
        Node {
            position_type: PositionType::Absolute,
            left: Val::Px(x),
            top: Val::Px(y + h - 26.0),
            width: Val::Px(w),
            ..default()
        },
        BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.55)),
        GlobalZIndex(5),
        ValueLabel,
        slot,
    ));
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
) -> Entity {
    commands
        .spawn((
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
        ))
        .id()
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
    let revealed = session.mtch.hand.community.len() + session.mtch.hand.dungeon.len();
    if revealed > reveal.0 {
        commands.spawn((AudioPlayer::new(audio.shuffle.clone()), PlaybackSettings::DESPAWN));
    }
    reveal.0 = revealed;
}

/// AI 玩家自动行动，用计时器拉开节奏，方便人类玩家旁观。
fn ai_auto_play(time: Res<Time>, mut delay: Local<f32>, mut session: ResMut<GameSession>) {
    let g = &mut session.mtch.hand;
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
    let g = &mut session.mtch.hand;
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
    selection: Option<Res<Selection>>,
    mut q: Query<(Option<&mut Text>, Option<&mut ImageNode>, Option<&mut BorderColor>, &Slot)>,
) {
    let m = &session.mtch;
    let g = &m.hand;
    // 点选阶段公共牌边框由 selection_ui_update 接管（高亮选中），这里不要覆盖。
    let selecting = selection.is_some();
    for (text, image, border, slot) in &mut q {
        // 同一个 Slot 可能既有图片牌位、又有数值文字位；下面三个 setter 各取所需。
        match slot {
            Slot::Status => set_text(text, status_text(m)),
            Slot::SeatName(seat) => set_text(text, seat_name_text(m, *seat)),
            Slot::Community(i) => {
                let (handle, col, val) = match g.community.get(*i) {
                    Some(c) => (assets.load(format!("cards/community/{}.png", c.art())), COL_GOLD, community_value(c)),
                    None => (back.0.clone(), COL_DIM, String::new()),
                };
                set_image(image, handle);
                if !selecting {
                    set_border(border, col);
                }
                set_text(text, val);
            }
            Slot::Dungeon(i) => {
                let (handle, col, val) = match g.dungeon.get(*i) {
                    Some(mon) => (assets.load(format!("cards/dungeon/{}.png", mon.art)), COL_RED, format!("T{}  H{}", mon.threat, mon.health)),
                    None => (back.0.clone(), COL_DIM, String::new()),
                };
                set_image(image, handle);
                set_border(border, col);
                set_text(text, val);
            }
            Slot::Hole { seat, idx } => {
                let (handle, col, val) = match g.players.get(*seat) {
                    None => (back.0.clone(), COL_DIM, String::new()),
                    Some(p) => {
                        // 隐藏信息：只亮出人类自己的底牌；摊牌与结算阶段(Showdown/Done)全亮。
                        let over = matches!(g.phase, Phase::Showdown | Phase::Done);
                        let reveal = !p.is_ai || over;
                        let a = &p.hole[*idx];
                        let (h, v) = if reveal {
                            (assets.load(format!("cards/community/{}.png", a.art)), format!("P{}  H{}", a.power, a.health))
                        } else {
                            (back.0.clone(), String::new())
                        };
                        let c = if *seat == g.to_act && !over { COL_ACTIVE } else { COL_HOLE };
                        (h, c, v)
                    }
                };
                set_image(image, handle);
                set_border(border, col);
                set_text(text, val);
            }
        }
    }
}

/// 公共牌的数值文字：角色显示战力/生命，装备显示绑定职业+加成，消耗品显示效果。
fn community_value(c: &game_core::CommunityCard) -> String {
    use game_core::CommunityCard::*;
    match c {
        Unit(a) => format!("P{}  H{}", a.power, a.health),
        Equip(k) => k.tag(),
        Consum(k) => k.tag().to_string(),
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

fn seat_name_text(m: &Match, seat: usize) -> String {
    let g = &m.hand;
    match g.players.get(seat) {
        Some(p) => {
            let here = if seat == g.to_act && g.phase != Phase::Showdown { " *" } else { "" };
            // 负债标注：欠了银行多少（来自持久座位）。
            let debt = m.seats.get(seat).map(|s| s.debt).unwrap_or(0);
            let debt_line = if debt > 0 { format!("\nDebt {}", debt) } else { String::new() };
            format!(
                "{}{}\nChips {}\nin {} | {:?}{}",
                p.name, here, p.chips, p.committed, p.status, debt_line
            )
        }
        None => "(empty seat)".into(),
    }
}

fn status_text(m: &Match) -> String {
    let g = &m.hand;
    let berserk = if g.boss_berserk { "  狂暴!" } else { "" };
    let mut s = format!(
        "Hand #{}\n{}\nPot: {}   Bet: {}\n惊动 {:.1}{}\nP=战力 H=生命 T=威胁\n",
        m.hand_no + 1,
        m.scene.label(),
        g.pot,
        g.current_bet,
        g.aggro,
        berserk,
    );
    if g.phase == Phase::Showdown || g.phase == Phase::Done {
        s.push_str("\n=== 摊牌结算见面板 ===");
    } else {
        let actor = g.players.get(g.to_act).map(|p| p.name.as_str()).unwrap_or("-");
        s.push_str(&format!("To act: {actor}\n[Q]Check [W]Call\n[E]Raise [R]Fold\n[Tab]账本 [Esc]菜单"));
    }
    s
}

// ============================ 多人联机（阶段 1：大厅）============================

/// 服务器地址。优先级：命令行参数 > 环境变量 RIVERBORN_SERVER > 默认公网 IP。
/// 本地调试用：`cargo run -p riverborn -- 127.0.0.1:7777`
/// （WSL 跑 Windows exe 时 `VAR=值 cargo.exe` 那种前缀不会把环境变量透传给 Windows 进程，用参数最稳）。
fn server_addr() -> SocketAddr {
    let raw = std::env::args()
        .nth(1)
        .or_else(|| std::env::var("RIVERBORN_SERVER").ok())
        .unwrap_or_else(|| format!("43.155.246.125:{DEFAULT_PORT}"));
    raw.parse().unwrap_or_else(|_| {
        error!("服务器地址应为 ip:port，收到 `{raw}`，回退默认公网地址");
        format!("43.155.246.125:{DEFAULT_PORT}").parse().unwrap()
    })
}

/// 进入联机：建立到服务器的连接，初始化会话状态。
fn mp_connect(mut commands: Commands) {
    let server = server_addr();
    let socket = match UdpSocket::bind("0.0.0.0:0") {
        Ok(s) => s,
        Err(e) => {
            error!("绑定本地 UDP 失败: {e}");
            return;
        }
    };
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    let client_id = now.as_nanos() as u64; // 随机化的客户端 id
    let auth = ClientAuthentication::Unsecure {
        server_addr: server,
        client_id,
        user_data: None,
        protocol_id: PROTOCOL_ID,
    };
    match NetcodeClientTransport::new(now, auth, socket) {
        Ok(transport) => {
            commands.insert_resource(RenetClient::new(ConnectionConfig::default()));
            commands.insert_resource(transport);
            commands.insert_resource(Net::default());
            info!("连接服务器 {server} ...");
        }
        Err(e) => error!("创建连接失败: {e}"),
    }
}

/// 离开联机：断开并清理资源。
fn mp_disconnect(mut commands: Commands, client: Option<ResMut<RenetClient>>) {
    if let Some(mut c) = client {
        c.disconnect();
    }
    commands.remove_resource::<RenetClient>();
    commands.remove_resource::<NetcodeClientTransport>();
    commands.remove_resource::<Net>();
}

/// 连接成功且名字已定后，发一次 Hello 报名。
fn mp_send_hello(client: Option<ResMut<RenetClient>>, net: Option<ResMut<Net>>) {
    let (Some(mut client), Some(mut net)) = (client, net) else { return };
    if net.greeted || net.name.is_empty() || !client.is_connected() {
        return;
    }
    client.send_message(DefaultChannel::ReliableOrdered, ClientMsg::Hello { name: net.name.clone() }.encode());
    net.greeted = true;
}

/// 收服务器消息，更新会话状态并切换子屏。
fn mp_receive(
    client: Option<ResMut<RenetClient>>,
    net: Option<ResMut<Net>>,
    mut mp_next: ResMut<NextState<MpState>>,
) {
    let (Some(mut client), Some(mut net)) = (client, net) else { return };
    while let Some(bytes) = client.receive_message(DefaultChannel::ReliableOrdered) {
        let Some(msg) = ServerMsg::decode(&bytes) else { continue };
        match msg {
            ServerMsg::Welcome { your_id } => net.my_id = Some(your_id),
            ServerMsg::RoomList { rooms } => net.rooms = rooms,
            ServerMsg::Joined { room } => {
                net.room = Some(room);
                net.status.clear();
                mp_next.set(MpState::Lobby);
            }
            ServerMsg::RoomUpdate { room } => net.room = Some(room),
            ServerMsg::Left => {
                net.room = None;
                mp_next.set(MpState::Menu);
            }
            ServerMsg::Error { text } => net.status = format!("⚠ {text}"),
            ServerMsg::GameStarting => net.status = "游戏即将开始（对局功能在阶段 2）".into(),
        }
    }
}

/// 把键盘输入收进当前子屏对应的输入框。
fn mp_text_input(state: Res<State<MpState>>, net: Option<ResMut<Net>>, mut ev: EventReader<KeyboardInput>) {
    let Some(mut net) = net else { return };
    match state.get() {
        MpState::NameEntry => type_into(&mut net.name_input, &mut ev, 16),
        MpState::Menu => type_into(&mut net.code_input, &mut ev, 8),
        _ => {}
    }
}

/// 通用文本输入：把按键收进字符串（含退格）。
fn type_into(s: &mut String, ev: &mut EventReader<KeyboardInput>, max: usize) {
    for e in ev.read() {
        if e.state != ButtonState::Pressed {
            continue;
        }
        match &e.logical_key {
            Key::Character(text) => {
                for ch in text.chars() {
                    if !ch.is_control() && s.chars().count() < max {
                        s.push(ch);
                    }
                }
            }
            Key::Space if s.chars().count() < max => s.push(' '),
            Key::Backspace => {
                s.pop();
            }
            _ => {}
        }
    }
}

/// 处理联机界面所有按钮。
fn mp_buttons(
    mut q: Query<(&Interaction, &MpButton, &mut BackgroundColor), Changed<Interaction>>,
    client: Option<ResMut<RenetClient>>,
    net: Option<ResMut<Net>>,
    mut mp_next: ResMut<NextState<MpState>>,
) {
    let (Some(mut client), Some(mut net)) = (client, net) else { return };
    for (interaction, btn, mut bg) in &mut q {
        match interaction {
            Interaction::Pressed => match btn {
                MpButton::Confirm => {
                    let name = net.name_input.trim().to_string();
                    net.name = if name.is_empty() { "Player".into() } else { name };
                    mp_next.set(MpState::Menu);
                }
                MpButton::Create(n) => {
                    let room_name = format!("{}的房间", net.name);
                    let msg = ClientMsg::CreateRoom { room_name, max_players: *n, fill_ai: true };
                    client.send_message(DefaultChannel::ReliableOrdered, msg.encode());
                }
                MpButton::Refresh => client.send_message(DefaultChannel::ReliableOrdered, ClientMsg::ListRooms.encode()),
                MpButton::Join => {
                    let code = net.code_input.trim().to_uppercase();
                    if !code.is_empty() {
                        client.send_message(DefaultChannel::ReliableOrdered, ClientMsg::JoinRoom { code }.encode());
                    }
                }
                MpButton::Start => client.send_message(DefaultChannel::ReliableOrdered, ClientMsg::StartGame.encode()),
                MpButton::Leave => client.send_message(DefaultChannel::ReliableOrdered, ClientMsg::LeaveRoom.encode()),
            },
            Interaction::Hovered => bg.0 = BTN_HOVER,
            Interaction::None => bg.0 = BTN_NORMAL,
        }
    }
}

/// 刷新联机界面里的动态文字（输入框 / 房间列表 / 房间信息 / 提示）。
fn mp_refresh_ui(net: Option<Res<Net>>, mut q: Query<(&mut Text, &MpText)>) {
    let Some(net) = net else { return };
    for (mut text, kind) in &mut q {
        text.0 = match kind {
            MpText::NameInput => format!("名字: {}_", net.name_input),
            MpText::CodeInput => format!("房间号: {}_", net.code_input),
            MpText::RoomList => rooms_text(&net.rooms),
            MpText::Lobby => lobby_text(&net),
            MpText::Status => net.status.clone(),
        };
    }
}

fn rooms_text(rooms: &[RoomInfo]) -> String {
    if rooms.is_empty() {
        return "（暂无公开房间，点「刷新列表」或「创建房间」）".into();
    }
    let mut s = String::from("公开房间（输入房间号加入）:\n");
    for r in rooms {
        s.push_str(&format!("  [{}] {}   {}/{}\n", r.code, r.name, r.players, r.max_players));
    }
    s
}

fn lobby_text(net: &Net) -> String {
    let Some(room) = &net.room else { return String::new() };
    let humans = room.players.len();
    let mut s = format!("房间 [{}]  {}\n人数 {}/{}\n\n", room.code, room.name, humans, room.max_players);
    for p in &room.players {
        let host = if p.id == room.host { "  (房主)" } else { "" };
        let me = if Some(p.id) == net.my_id { "  ←你" } else { "" };
        s.push_str(&format!("· {}{}{}\n", p.name, host, me));
    }
    if room.fill_ai && humans < room.max_players {
        s.push_str(&format!("\n（开局将补 {} 个 AI 凑满 {} 人）", room.max_players - humans, room.max_players));
    }
    s
}

fn mp_button(parent: &mut ChildBuilder, action: MpButton, label: &str) {
    parent
        .spawn((
            Button,
            action,
            Node {
                width: Val::Px(220.0),
                height: Val::Px(56.0),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                border: UiRect::all(Val::Px(2.0)),
                ..default()
            },
            BorderColor(Color::srgb(0.45, 0.48, 0.58)),
            BackgroundColor(BTN_NORMAL),
        ))
        .with_children(|b| {
            b.spawn((
                Text::new(label),
                TextFont { font_size: 24.0, ..default() },
                TextColor(Color::srgb(0.92, 0.94, 0.97)),
            ));
        });
}

/// 联机子屏的根容器（竖排居中 + MpUi 标记）。
fn mp_root(commands: &mut Commands) -> Entity {
    commands
        .spawn((
            Node {
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                row_gap: Val::Px(18.0),
                ..default()
            },
            BackgroundColor(Color::srgb(0.08, 0.09, 0.12)),
            MpUi,
        ))
        .id()
}

fn title(parent: &mut ChildBuilder, text: &str) {
    parent.spawn((
        Text::new(text.to_string()),
        TextFont { font_size: 40.0, ..default() },
        TextColor(Color::srgb(0.95, 0.85, 0.55)),
    ));
}

fn mp_label(parent: &mut ChildBuilder, kind: MpText, size: f32) {
    parent.spawn((
        Text::new(""),
        TextFont { font_size: size, ..default() },
        TextColor(Color::srgb(0.88, 0.90, 0.94)),
        kind,
    ));
}

fn enter_name_entry(mut commands: Commands) {
    let root = mp_root(&mut commands);
    commands.entity(root).with_children(|root| {
        title(root, "联机 · 输入名字");
        mp_label(root, MpText::NameInput, 30.0);
        mp_button(root, MpButton::Confirm, "确定");
        mp_label(root, MpText::Status, 20.0);
        root.spawn((
            Text::new("打字输入名字，[退格]删除，[Esc]返回主菜单"),
            TextFont { font_size: 18.0, ..default() },
            TextColor(Color::srgb(0.55, 0.58, 0.65)),
        ));
    });
}

fn enter_mp_menu(mut commands: Commands) {
    let root = mp_root(&mut commands);
    commands.entity(root).with_children(|root| {
        title(root, "大厅");
        root.spawn((
            Text::new("创建房间 · 选人数（人不够，房主开局时自动补 AI）:"),
            TextFont { font_size: 20.0, ..default() },
            TextColor(Color::srgb(0.82, 0.84, 0.88)),
        ));
        root.spawn(Node { flex_direction: FlexDirection::Row, column_gap: Val::Px(14.0), ..default() })
            .with_children(|row| {
                for n in 2..=6usize {
                    mp_num_button(row, n);
                }
            });
        mp_button(root, MpButton::Refresh, "刷新房间列表");
        mp_label(root, MpText::RoomList, 24.0);
        mp_label(root, MpText::CodeInput, 28.0);
        mp_button(root, MpButton::Join, "加入");
        mp_label(root, MpText::Status, 20.0);
        root.spawn((
            Text::new("打字输入房间号，[Esc]返回主菜单"),
            TextFont { font_size: 18.0, ..default() },
            TextColor(Color::srgb(0.55, 0.58, 0.65)),
        ));
    });
}

/// 选人数建房的方块按钮（点一下就建对应人数的房间）。
fn mp_num_button(parent: &mut ChildBuilder, n: usize) {
    parent
        .spawn((
            Button,
            MpButton::Create(n),
            Node {
                width: Val::Px(84.0),
                height: Val::Px(64.0),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                border: UiRect::all(Val::Px(2.0)),
                ..default()
            },
            BorderColor(Color::srgb(0.45, 0.48, 0.58)),
            BackgroundColor(BTN_NORMAL),
        ))
        .with_children(|b| {
            b.spawn((
                Text::new(format!("{n}")),
                TextFont { font_size: 34.0, ..default() },
                TextColor(Color::srgb(0.92, 0.94, 0.97)),
            ));
        });
}

fn enter_lobby(mut commands: Commands) {
    let root = mp_root(&mut commands);
    commands.entity(root).with_children(|root| {
        title(root, "房间");
        mp_label(root, MpText::Lobby, 26.0);
        root.spawn(Node { flex_direction: FlexDirection::Row, column_gap: Val::Px(20.0), ..default() })
            .with_children(|row| {
                mp_button(row, MpButton::Start, "开始(房主)");
                mp_button(row, MpButton::Leave, "离开");
            });
        mp_label(root, MpText::Status, 20.0);
    });
}

fn cleanup_mp(mut commands: Commands, q: Query<Entity, With<MpUi>>) {
    for e in &q {
        commands.entity(e).despawn_recursive();
    }
}

// ============================ 设置（占位）============================

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

// ==================== 结算面板 / 借锅 / 记账 / 场景背景 ====================

/// 摊牌驱动：人类还在手里就先让他点选 3 张公共牌，确认后结算；
/// 已弃牌/无人类则自动结算。结算完成后弹结算面板。每手只弹一次。
fn showdown_director(
    mut commands: Commands,
    mut session: ResMut<GameSession>,
    selection: Option<Res<Selection>>,
    result_ui: Query<(), With<ResultUi>>,
) {
    if !session.mtch.hand_over() || !result_ui.is_empty() {
        return;
    }
    // 人类还没弃牌且有公共牌可选 → 走点选流程。
    let human_in = session
        .mtch
        .hand
        .players
        .iter()
        .find(|p| !p.is_ai)
        .map(|p| p.is_in_hand())
        .unwrap_or(false);
    let needs_pick = human_in && session.mtch.hand.community.len() >= 3;

    if needs_pick && !session.mtch.settled {
        // 进入/保持点选阶段，等玩家点「确认组队」（由 confirm_selection 结算）。
        if selection.is_none() {
            commands.insert_resource(Selection::default());
            spawn_select_ui(&mut commands);
        }
        return;
    }

    // 无需点选（已弃牌/无人类）→ 自动选最优并结算。
    if !session.mtch.settled {
        session.mtch.settle_hand(&[]);
    }
    spawn_result_panel(&mut commands, &session.mtch);
}

/// 点选阶段的提示 + 「确认组队」按钮（放在左下空白处，不挡公共牌）。
fn spawn_select_ui(commands: &mut Commands) {
    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(290.0),
                top: Val::Px(936.0),
                width: Val::Px(380.0),
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(8.0),
                ..default()
            },
            GlobalZIndex(40),
            SelectUi,
        ))
        .with_children(|root| {
            root.spawn((
                Text::new("点选 3 张公共牌组队  (已选 0/3)"),
                TextFont { font_size: 20.0, ..default() },
                TextColor(Color::srgb(0.95, 0.85, 0.55)),
                SelectCountText,
            ));
            root.spawn((
                Button,
                ConfirmButton,
                Node {
                    width: Val::Px(240.0),
                    height: Val::Px(52.0),
                    justify_content: JustifyContent::Center,
                    align_items: AlignItems::Center,
                    border: UiRect::all(Val::Px(2.0)),
                    ..default()
                },
                BorderColor(Color::srgb(0.45, 0.48, 0.58)),
                BackgroundColor(BTN_NORMAL),
            ))
            .with_children(|b| {
                b.spawn((
                    Text::new("确认组队"),
                    TextFont { font_size: 24.0, ..default() },
                    TextColor(Color::srgb(0.92, 0.94, 0.97)),
                ));
            });
        });
}

/// 地牢节点档位中文名。
fn tier_name(kind: game_core::MonsterKind) -> &'static str {
    use game_core::MonsterKind::*;
    match kind {
        Goblin => "小怪",
        Elite | PoisonSwamp => "精英",
        Boss => "Boss",
        Treasure => "宝箱",
    }
}

/// 弹出本手结算计分板（带「下一手」按钮）。读 `mtch.last` 的结算结果。
/// 计分板显示：地牢各节点有效威胁/生命合计、每个玩家队伍 P/H、通关与否、战后优势值，
/// 让玩家看懂自己是怎么团灭的（战力压不过哪关、或生命被扣穿）。
fn spawn_result_panel(commands: &mut Commands, m: &Match) {
    let Some(settle) = m.last.clone() else { return };
    let g = &m.hand;

    // 地牢汇总：各节点经惊动/狂暴后的有效威胁 + 合计。
    let mut dungeon_line = String::from("地牢  ");
    let mut total_t = 0u32;
    for (i, node) in g.dungeon.iter().enumerate() {
        let t = game_core::combat::node_threat(node, g.aggro, g.boss_berserk);
        total_t += t;
        if i > 0 {
            dungeon_line.push_str("  |  ");
        }
        dungeon_line.push_str(&format!("{} T{}", tier_name(node.kind), t));
    }
    let total_h: u32 = g.dungeon.iter().map(|n| n.health).sum();
    dungeon_line.push_str(&format!("     威胁合计 {}   生命合计 {}", total_t, total_h));
    if g.aggro > 0.0 {
        dungeon_line.push_str(&format!("   惊动 {:.1}{}", g.aggro, if g.boss_berserk { " 狂暴!" } else { "" }));
    }

    // 计分板：每个玩家一行（队伍 P/H → 结果 + 优势）。
    let mut board = String::new();
    for r in &settle.results {
        let nm = m.seats.iter().find(|s| s.id == r.id).map(|s| s.name.as_str()).unwrap_or("?");
        if r.cleared {
            board.push_str(&format!(
                "{:<11} 队伍 P{:<3} H{:<3} →  通关  余血{} 战力{}   优势 {}\n",
                nm, r.team_power, r.team_health, r.remaining_health, r.remaining_power, r.advantage
            ));
        } else {
            board.push_str(&format!(
                "{:<11} 队伍 P{:<3} H{:<3} →  团灭\n",
                nm, r.team_power, r.team_health
            ));
        }
    }

    let winner_line = match settle.winner {
        Some(id) => {
            let name = m.seats.iter().find(|s| s.id == id).map(|s| s.name.as_str()).unwrap_or("?");
            format!("赢家：{}    底池 {}", name, g.pot)
        }
        None => format!("无人通关，底池 {} 滚入下一手", g.pot),
    };

    // 外层铺满整窗但**透明**，只负责居中；这样四周座位亮出的底牌仍然可见。
    // 计分板内容收在中间一个半透明框里。
    commands
        .spawn((
            Node {
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                ..default()
            },
            GlobalZIndex(50),
            ResultUi,
        ))
        .with_children(|root| {
            root.spawn((
                Node {
                    flex_direction: FlexDirection::Column,
                    align_items: AlignItems::Center,
                    row_gap: Val::Px(14.0),
                    padding: UiRect::all(Val::Px(28.0)),
                    border: UiRect::all(Val::Px(2.0)),
                    ..default()
                },
                BackgroundColor(Color::srgba(0.04, 0.05, 0.08, 0.92)),
                BorderColor(Color::srgb(0.55, 0.50, 0.35)),
            ))
            .with_children(|panel| {
                panel.spawn((
                    Text::new(format!("本手结算 · {}", m.scene.label())),
                    TextFont { font_size: 40.0, ..default() },
                    TextColor(Color::srgb(0.95, 0.85, 0.55)),
                ));
                panel.spawn((
                    Text::new(dungeon_line),
                    TextFont { font_size: 22.0, ..default() },
                    TextColor(Color::srgb(0.88, 0.55, 0.50)),
                ));
                panel.spawn((
                    Text::new(board),
                    TextFont { font_size: 23.0, ..default() },
                    TextColor(Color::srgb(0.88, 0.90, 0.94)),
                    Node { margin: UiRect::vertical(Val::Px(6.0)), ..default() },
                ));
                panel.spawn((
                    Text::new(winner_line),
                    TextFont { font_size: 28.0, ..default() },
                    TextColor(Color::srgb(0.95, 0.85, 0.55)),
                ));
                panel.spawn((
                    Button,
                    NextHandButton,
                    Node {
                        width: Val::Px(280.0),
                        height: Val::Px(64.0),
                        justify_content: JustifyContent::Center,
                        align_items: AlignItems::Center,
                        border: UiRect::all(Val::Px(2.0)),
                        ..default()
                    },
                    BorderColor(Color::srgb(0.45, 0.48, 0.58)),
                    BackgroundColor(BTN_NORMAL),
                ))
                .with_children(|b| {
                    b.spawn((
                        Text::new("下一手"),
                        TextFont { font_size: 28.0, ..default() },
                        TextColor(Color::srgb(0.92, 0.94, 0.97)),
                    ));
                });
            });
        });
}

/// 点选阶段：点公共牌切换选中（最多 3 张）。
fn selection_clicks(
    selection: Option<ResMut<Selection>>,
    q: Query<(&Interaction, &Selectable), Changed<Interaction>>,
) {
    let Some(mut sel) = selection else { return };
    for (interaction, s) in &q {
        if *interaction == Interaction::Pressed {
            if let Some(pos) = sel.picks.iter().position(|&x| x == s.0) {
                sel.picks.remove(pos);
            } else if sel.picks.len() < 3 {
                sel.picks.push(s.0);
            }
        }
    }
}

/// 点选阶段：高亮已选公共牌、刷新计数文字与确认按钮颜色。
fn selection_ui_update(
    selection: Option<Res<Selection>>,
    mut cards: Query<(&Selectable, &mut BorderColor)>,
    mut count: Query<&mut Text, With<SelectCountText>>,
    mut confirm: Query<&mut BackgroundColor, With<ConfirmButton>>,
) {
    let Some(sel) = selection else { return };
    for (s, mut border) in &mut cards {
        border.0 = if sel.picks.contains(&s.0) { COL_SELECT } else { COL_GOLD };
    }
    if let Ok(mut t) = count.get_single_mut() {
        t.0 = format!("点选 3 张公共牌组队  (已选 {}/3)", sel.picks.len());
    }
    if let Ok(mut bg) = confirm.get_single_mut() {
        bg.0 = if sel.picks.len() == 3 { Color::srgb(0.20, 0.40, 0.22) } else { BTN_NORMAL };
    }
}

/// 「确认组队」：用人类手选的 3 张结算，关掉点选 UI（结算面板由 director 弹）。
fn confirm_selection(
    mut commands: Commands,
    mut session: ResMut<GameSession>,
    selection: Option<Res<Selection>>,
    q: Query<&Interaction, (Changed<Interaction>, With<ConfirmButton>)>,
    select_ui: Query<Entity, With<SelectUi>>,
) {
    let Some(sel) = selection else { return };
    if sel.picks.len() != 3 {
        return;
    }
    for interaction in &q {
        if *interaction == Interaction::Pressed {
            let picks = [sel.picks[0], sel.picks[1], sel.picks[2]];
            // 单机人类是唯一非 AI 座位。
            let human = session.mtch.seats.iter().find(|s| !s.is_ai).map(|s| s.id);
            if let Some(id) = human {
                session.mtch.settle_hand(&[(id, picks)]);
            } else {
                session.mtch.settle_hand(&[]);
            }
            commands.remove_resource::<Selection>();
            for e in &select_ui {
                commands.entity(e).despawn_recursive();
            }
        }
    }
}

/// 「下一手」按钮：发新一手并关掉结算面板。
fn result_buttons(
    mut commands: Commands,
    mut session: ResMut<GameSession>,
    mut q: Query<(&Interaction, &mut BackgroundColor), (Changed<Interaction>, With<NextHandButton>)>,
    panel: Query<Entity, With<ResultUi>>,
) {
    for (interaction, mut bg) in &mut q {
        match interaction {
            Interaction::Pressed => {
                session.mtch.next_hand();
                for e in &panel {
                    commands.entity(e).despawn_recursive();
                }
            }
            Interaction::Hovered => bg.0 = BTN_HOVER,
            Interaction::None => bg.0 = BTN_NORMAL,
        }
    }
}

/// 「借一锅」按钮：人类（座位 0）筹码偏低时显示，点一次借一锅。
fn borrow_button(
    mut session: ResMut<GameSession>,
    mut was_pressed: Local<bool>,
    mut q: Query<(&Interaction, &mut Node, &mut BackgroundColor), With<BorrowButton>>,
) {
    let low = {
        let m = &session.mtch;
        let chips = m.seats.first().map(|s| s.chips).unwrap_or(0);
        let need = m
            .hand
            .current_bet
            .saturating_sub(m.hand.players.first().map(|p| p.committed).unwrap_or(0));
        chips < need.max(100)
    };
    for (interaction, mut node, mut bg) in &mut q {
        node.display = if low { Display::Flex } else { Display::None };
        let pressed = *interaction == Interaction::Pressed;
        match interaction {
            Interaction::Pressed => {
                if low && !*was_pressed {
                    session.mtch.borrow(0);
                }
                bg.0 = Color::srgb(0.32, 0.25, 0.14);
            }
            Interaction::Hovered => bg.0 = Color::srgb(0.26, 0.21, 0.13),
            Interaction::None => bg.0 = Color::srgb(0.20, 0.16, 0.10),
        }
        *was_pressed = pressed;
    }
}

/// Tab 开/关记账面板。
fn ledger_toggle(
    mut commands: Commands,
    keys: Res<ButtonInput<KeyCode>>,
    open: Query<Entity, With<LedgerUi>>,
) {
    if !keys.just_pressed(KeyCode::Tab) {
        return;
    }
    if let Some(e) = open.iter().next() {
        commands.entity(e).despawn_recursive();
        return;
    }
    commands
        .spawn((
            Node {
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                row_gap: Val::Px(16.0),
                ..default()
            },
            BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.85)),
            GlobalZIndex(60),
            LedgerUi,
        ))
        .with_children(|root| {
            root.spawn((
                Text::new("记账面板 · 按净身价排名（Tab 关闭）"),
                TextFont { font_size: 36.0, ..default() },
                TextColor(Color::srgb(0.95, 0.85, 0.55)),
            ));
            root.spawn((
                Text::new(""),
                TextFont { font_size: 26.0, ..default() },
                TextColor(Color::srgb(0.90, 0.92, 0.96)),
                LedgerText,
            ));
        });
}

/// 记账面板打开时持续刷新内容。
fn update_ledger(session: Res<GameSession>, mut q: Query<&mut Text, With<LedgerText>>) {
    if let Ok(mut text) = q.get_single_mut() {
        text.0 = ledger_body(&session.mtch);
    }
}

/// 按净身价排名，列出每个座位的筹码 / 负债 / 净身价 / 水位。
fn ledger_body(m: &Match) -> String {
    let mut s = String::from("  名字            筹码     负债   净身价    水位\n");
    for (rank, &i) in m.ranking().iter().enumerate() {
        let seat = &m.seats[i];
        let net = seat.net_worth();
        let profit = seat.profit();
        let tide = if profit > 0 {
            format!("+{} 水上", profit)
        } else if profit < 0 {
            format!("{} 水下", profit)
        } else {
            "持平".to_string()
        };
        s.push_str(&format!(
            "{}. {:<12}  {:>6}   {:>5}   {:>6}   {}\n",
            rank + 1,
            seat.name,
            seat.chips,
            seat.debt,
            net,
            tide
        ));
    }
    s
}

/// 每手随机场景 → 切换牌桌背景图（仅在场景变化时换，避免每帧重置）。
fn sync_scene_bg(
    session: Res<GameSession>,
    assets: Res<AssetServer>,
    mut last: Local<Option<&'static str>>,
    mut q: Query<&mut ImageNode, With<SceneBackground>>,
) {
    let art = session.mtch.scene.art();
    if *last == Some(art) {
        return;
    }
    *last = Some(art);
    for mut img in &mut q {
        img.image = assets.load(format!("table/{}.png", art));
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
