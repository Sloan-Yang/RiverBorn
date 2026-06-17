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
use bevy::input::mouse::MouseWheel;
use bevy::input::ButtonState;
use bevy::prelude::*;
use bevy::ui::RelativeCursorPosition;
use bevy::window::PrimaryWindow;
use bevy::winit::WinitWindows;
use bevy_renet::netcode::{ClientAuthentication, NetcodeClientPlugin, NetcodeClientTransport};
use bevy_renet::renet::{ConnectionConfig, DefaultChannel, RenetClient};
use bevy_renet::RenetClientPlugin;
use game_core::{Action, Match, Phase, SeatDef};
use riverborn_net::{
    CardView, ClientMsg, GameView, PlayerId, RoomInfo, RoomState, ServerMsg, DEFAULT_PORT, PROTOCOL_ID,
};
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
    InGame,
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
    view: Option<GameView>, // 对局中：服务器下发的最新视图
}

/// 联机各子屏的根标记（OnExit 清理）。
#[derive(Component)]
struct MpUi;

/// 联机的 BGM 实体（大厅曲 / 对局曲切换时用它定位旧曲）。
#[derive(Component)]
struct MpMusic;

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

/// 每张牌位的动画状态：记录布局基准坐标/尺寸，由 animate_cards 据悬停 + 发牌重算 Node。
/// 不能直接读 Node 当基准——Node 每帧被本动画改写，必须留一份原始布局值。
#[derive(Component)]
struct Card {
    left: f32,
    top: f32,
    w: f32,
    h: f32,
    lift: f32,      // 悬停抬升进度 0..1（平滑趋近）
    deal: f32,      // 发牌/翻牌弹跳余量 1..0
    was_face: bool, // 上帧是否已亮牌面（用来识别「刚翻开」触发发牌动画）
}

impl Card {
    fn new(left: f32, top: f32, w: f32, h: f32) -> Self {
        Card { left, top, w, h, lift: 0.0, deal: 0.0, was_face: false }
    }
}

/// 底池金额的显示状态（全局；SP/MP 各自每帧写 target）。
#[derive(Resource, Default)]
struct PotState {
    shown: f32,      // 平滑显示中的数值
    target: u32,     // 真实底池
    last_target: u32, // 上次的 target，用来识别「底池涨了」
    flash: f32,      // 涨钱时的高亮脉冲 1..0
    chip_tier: Option<usize>, // 当前筹码堆已渲染的枚数（变了才重建）
}

/// 底池面板根 / 金额文字 / 筹码堆容器 / 落入底池的筹码动画。
#[derive(Component)]
struct PotDisplayRoot;
#[derive(Component)]
struct PotAmountText;
#[derive(Component)]
struct PotChipStack;
#[derive(Component)]
struct PotChip {
    timer: Timer,
    from_top: f32,
    to_top: f32,
}

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

const PLAYER_DOCK_X: f32 = 16.0;
const PLAYER_DOCK_Y: f32 = 874.0;
const PLAYER_DOCK_W: f32 = W - PLAYER_DOCK_X * 2.0;
const PLAYER_DOCK_H: f32 = 190.0;
const PLAYER_AVATAR_X: f32 = 36.0;
const PLAYER_AVATAR_Y: f32 = PLAYER_DOCK_Y + (PLAYER_DOCK_H - AVATAR_SIZE) / 2.0;
const PLAYER_STATUS_X: f32 = PLAYER_AVATAR_X + AVATAR_SIZE + 14.0;
const PLAYER_STATUS_Y: f32 = PLAYER_DOCK_Y + 38.0;
const PLAYER_ACTION_X: f32 = 380.0;
const PLAYER_ACTION_Y: f32 = PLAYER_DOCK_Y + 44.0;
const PLAYER_ACTION_W: f32 = 920.0;
const PLAYER_ACTION_H: f32 = 116.0;
const PLAYER_HOLE_CARD_H: f32 = 154.0;
const PLAYER_HOLE_CARD_W: f32 = PLAYER_HOLE_CARD_H * CARD_ASPECT;
const PLAYER_HOLE_X: f32 = W - 36.0 - PLAYER_HOLE_CARD_W * 2.0 - HOLE_CARD_GAP;
const PLAYER_HOLE_Y: f32 = PLAYER_DOCK_Y + (PLAYER_DOCK_H - PLAYER_HOLE_CARD_H) / 2.0;
const BORROW_BUTTON_X: f32 = 34.0;
const BORROW_BUTTON_Y: f32 = PLAYER_DOCK_Y - 68.0;

// 底池显示（牌桌中央、地牢池与玩家 dock 之间的空档）。
const POT_W: f32 = 320.0;
const POT_H: f32 = 92.0;
const POT_X: f32 = (W - POT_W) / 2.0;
const POT_Y: f32 = 752.0;
const CHIP_D: f32 = 30.0; // 单枚筹码直径

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
        .insert_resource(PotState::default())
        .add_plugins((RenetClientPlugin, NetcodeClientPlugin))
        .init_state::<AppState>()
        .add_sub_state::<PauseState>()
        .add_sub_state::<MpState>()
        .add_event::<BetIntent>()
        .add_systems(Startup, (setup, set_window_icon))
        .add_systems(Update, apply_ui_font)
        // 下注 HUD（底部 4 按钮 + 加注拉杆 + 特效）：单机/联机共用，全程跑（自身判活）
        .add_systems(
            Update,
            (bet_keys, bet_hud_buttons, bet_slider_drag, bet_button_hover, bet_hud_update, bet_effects, floaters),
        )
        // 牌面动画 + 底池显示：全程跑（无对应实体时自动空转）
        .add_systems(Update, (animate_cards, pot_display, chip_drops))
        .add_systems(Update, bet_ctx_sp.run_if(in_state(PauseState::Running)))
        .add_systems(Update, apply_bet_sp.run_if(in_state(PauseState::Running)))
        .add_systems(Update, update_pot_sp.run_if(in_state(AppState::SinglePlayer)))
        .add_systems(Update, bet_ctx_mp.run_if(in_state(MpState::InGame)))
        .add_systems(Update, send_bet_mp.run_if(in_state(MpState::InGame)))
        .add_systems(Update, update_pot_mp.run_if(in_state(MpState::InGame)))
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
            (ai_auto_play, deal_sfx).chain().run_if(in_state(PauseState::Running)),
        )
        // 画面刷新单人全程都跑（暂停时也要显示牌桌）
        .add_systems(Update, refresh.run_if(in_state(AppState::SinglePlayer)))
        // 摊牌结算面板 / 借锅 / 记账 / 场景背景：单人全程
        .add_systems(
            Update,
            (
                showdown_director,
                confirm_selection,
                result_buttons,
                borrow_button,
                ledger_toggle,
                update_ledger,
                sync_scene_bg,
            )
                .run_if(in_state(AppState::SinglePlayer)),
        )
        // 点选公共牌的高亮/计数：单机和联机共用，只要 Selection 资源在就跑
        .add_systems(
            Update,
            (selection_clicks, selection_ui_update).run_if(resource_exists::<Selection>),
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
        // 联机对局
        .add_systems(OnEnter(MpState::InGame), enter_mp_game)
        .add_systems(OnExit(MpState::InGame), cleanup)
        .add_systems(
            Update,
            (mp_game_refresh, mp_game_overlays, mp_game_buttons, mp_confirm, mp_deal_sfx)
                .run_if(in_state(MpState::InGame)),
        )
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
    commands.insert_resource(Bet::default());
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

/// 对手座位坐标。底部留给本地玩家 dock，不再放其它座位。
fn opponent_positions(count: usize) -> Vec<(f32, f32)> {
    const TOP: (f32, f32) = (1000.0, 28.0);
    const TOP_LEFT: (f32, f32) = (520.0, 56.0);
    const TOP_RIGHT: (f32, f32) = (1390.0, 56.0);
    const MID_LEFT: (f32, f32) = (24.0, 260.0);
    const MID_RIGHT: (f32, f32) = (1700.0, 260.0);
    const LOW_LEFT: (f32, f32) = (24.0, 560.0);
    const LOW_RIGHT: (f32, f32) = (1700.0, 560.0);
    match count {
        0 => vec![],
        1 => vec![TOP],
        2 => vec![MID_LEFT, MID_RIGHT],
        3 => vec![TOP, MID_LEFT, MID_RIGHT],
        4 => vec![TOP_LEFT, TOP_RIGHT, MID_LEFT, MID_RIGHT],
        _ => vec![TOP, TOP_LEFT, TOP_RIGHT, LOW_LEFT, LOW_RIGHT],
    }
}

/// 返回 (seat index, avatar x, avatar y)。本地玩家固定在底部 dock，其余座位围绕牌桌。
fn seat_layout(count: usize, local_seat: usize) -> Vec<(usize, f32, f32)> {
    if count == 0 {
        return Vec::new();
    }
    let local = local_seat.min(count - 1);
    let mut layout = vec![(local, PLAYER_AVATAR_X, PLAYER_AVATAR_Y)];
    let opponents = opponent_positions(count.saturating_sub(1));
    for (seat, (x, y)) in (0..count).filter(|&seat| seat != local).zip(opponents) {
        layout.push((seat, x, y));
    }
    layout
}

fn spawn_player_dock(commands: &mut Commands) {
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            left: Val::Px(PLAYER_DOCK_X),
            top: Val::Px(PLAYER_DOCK_Y),
            width: Val::Px(PLAYER_DOCK_W),
            height: Val::Px(PLAYER_DOCK_H),
            border: UiRect::all(Val::Px(2.0)),
            ..default()
        },
        BorderColor(Color::srgba(0.68, 0.72, 0.82, 0.35)),
        BackgroundColor(Color::srgba(0.03, 0.04, 0.06, 0.76)),
    ));
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

    // 6) 座位：本地玩家固定在底部 dock；对手围绕牌桌。
    spawn_player_dock(&mut commands);
    for (seat, x, y) in seat_layout(session.mtch.seats.len(), 0) {
        let local = seat == 0;
        spawn_avatar(&mut commands, x, y, assets.load(avatar_file(seat)));
        spawn_seat_name(
            &mut commands,
            if local { PLAYER_STATUS_X } else { x + SEAT_TEXT_X_OFFSET },
            if local { PLAYER_STATUS_Y } else { y },
            seat,
        );
        let (card_w, card_h, x0, cy) = if local {
            (PLAYER_HOLE_CARD_W, PLAYER_HOLE_CARD_H, PLAYER_HOLE_X, PLAYER_HOLE_Y)
        } else {
            (HOLE_CARD_W, HOLE_CARD_H, x, y + HOLE_CARD_TOP_OFFSET)
        };
        let x1 = x0 + card_w + HOLE_CARD_GAP;
        spawn_card(&mut commands, x0, cy, card_w, card_h, COL_HOLE, card_back.clone(), Slot::Hole { seat, idx: 0 });
        spawn_value_label(&mut commands, x0, cy, card_w, card_h, Slot::Hole { seat, idx: 0 });
        spawn_card(&mut commands, x1, cy, card_w, card_h, COL_HOLE, card_back.clone(), Slot::Hole { seat, idx: 1 });
        spawn_value_label(&mut commands, x1, cy, card_w, card_h, Slot::Hole { seat, idx: 1 });
    }

    // 7) 「借一锅」按钮：默认隐藏，borrow_button 系统按人类筹码情况显隐。
    //    放在底部 dock 上方，不占用主要操作区。
    commands
        .spawn((
            Button,
            BorrowButton,
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(BORROW_BUTTON_X),
                top: Val::Px(BORROW_BUTTON_Y),
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
    spawn_bet_hud(&mut commands);
    spawn_pot_display(&mut commands);
    commands.insert_resource(PotState::default()); // 新局重置底池显示（含筹码堆重建标记）
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
            // 悬停弹出 / 发牌动画所需：可交互 + 可调 z + 描边光晕 + 基准坐标。
            Button,
            Interaction::default(),
            GlobalZIndex(0),
            Outline { width: Val::Px(0.0), offset: Val::Px(2.0), color: COL_SELECT },
            Card::new(x, y, w, h),
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
            max_width: Val::Px(230.0),
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

/// 统一刷新所有槽位（文字槽改字、图片槽换图/换边框色）。
fn refresh(
    session: Res<GameSession>,
    back: Res<CardBack>,
    assets: Res<AssetServer>,
    selection: Option<Res<Selection>>,
    mut q: Query<(Option<&mut Text>, Option<&mut ImageNode>, Option<&mut BorderColor>, Option<&mut Card>, &Slot)>,
) {
    let m = &session.mtch;
    let g = &m.hand;
    // 点选阶段公共牌边框由 selection_ui_update 接管（高亮选中），这里不要覆盖。
    let selecting = selection.is_some();
    for (text, image, border, card, slot) in &mut q {
        // 同一个 Slot 可能既有图片牌位、又有数值文字位；下面三个 setter 各取所需。
        match slot {
            Slot::Status => set_text(text, status_text(m)),
            Slot::SeatName(seat) => set_text(text, seat_name_text(m, *seat)),
            Slot::Community(i) => {
                let face = g.community.get(*i);
                let (handle, col, val) = match face {
                    Some(c) => (assets.load(format!("cards/community/{}.png", c.art())), COL_GOLD, community_value(c)),
                    None => (back.0.clone(), COL_DIM, String::new()),
                };
                set_image(image, handle);
                if !selecting {
                    set_border(border, col);
                }
                set_text(text, val);
                note_reveal(card, face.is_some());
            }
            Slot::Dungeon(i) => {
                let face = g.dungeon.get(*i);
                let (handle, col, val) = match face {
                    Some(mon) => (assets.load(format!("cards/dungeon/{}.png", mon.art)), COL_RED, format!("T{}  H{}", mon.threat, mon.health)),
                    None => (back.0.clone(), COL_DIM, String::new()),
                };
                set_image(image, handle);
                set_border(border, col);
                set_text(text, val);
                note_reveal(card, face.is_some());
            }
            Slot::Hole { seat, idx } => {
                let mut revealed = false;
                let (handle, col, val) = match g.players.get(*seat) {
                    None => (back.0.clone(), COL_DIM, String::new()),
                    Some(p) => {
                        // 隐藏信息：只亮出人类自己的底牌；摊牌与结算阶段(Showdown/Done)全亮。
                        let over = matches!(g.phase, Phase::Showdown | Phase::Done);
                        let reveal = !p.is_ai || over;
                        revealed = reveal;
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
                note_reveal(card, revealed);
            }
        }
    }
}

/// 牌位由「盖着」翻成「亮面」时，触发一次发牌弹跳动画。
fn note_reveal(card: Option<Mut<Card>>, is_face: bool) {
    if let Some(mut c) = card {
        if is_face && !c.was_face {
            c.deal = 1.0;
        }
        c.was_face = is_face;
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
fn mp_connect(mut commands: Commands, assets: Res<AssetServer>) {
    // 大厅 BGM（循环），离开联机时由 cleanup 停掉。
    commands.spawn((
        AudioPlayer::new(assets.load("audio/Main_menu_music.mp3")),
        PlaybackSettings::LOOP,
        MpMusic,
    ));
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
    state: Res<State<MpState>>,
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
            ServerMsg::GameStarting => net.status = "发牌中…".into(),
            ServerMsg::View { view } => {
                net.view = Some(view);
                // 收到第一份对局视图就切到牌桌。
                if *state.get() != MpState::InGame {
                    mp_next.set(MpState::InGame);
                }
            }
            ServerMsg::GameEnded => {
                net.view = None;
                mp_next.set(MpState::Lobby);
            }
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

fn enter_lobby(mut commands: Commands, assets: Res<AssetServer>, music: Query<(), With<MpMusic>>) {
    // 从对局返回大厅时（对局 BGM 已被清理），重新放大厅 BGM。
    if music.is_empty() {
        commands.spawn((
            AudioPlayer::new(assets.load("audio/Main_menu_music.mp3")),
            PlaybackSettings::LOOP,
            MpMusic,
        ));
    }
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

// ===================== 联机对局（阶段 2）：从 GameView 渲染牌桌 =====================

/// 联机牌桌头像：你自己用 player.png，其余按座位循环用 AI 立绘。
fn mp_avatar_file(seat: usize, you_seat: usize) -> String {
    if seat == you_seat {
        "avatars/player.png".to_string()
    } else {
        format!("avatars/{}.png", AI_AVATARS[seat % AI_AVATARS.len()])
    }
}

/// 进入对局：按视图里的人数铺牌桌（复用单机的牌槽 Slot + spawn 助手）。
fn enter_mp_game(
    mut commands: Commands,
    assets: Res<AssetServer>,
    audio: Res<AudioAssets>,
    net: Option<Res<Net>>,
    music: Query<Entity, With<MpMusic>>,
) {
    let Some(net) = net else { return };
    let Some(view) = &net.view else { return };
    let count = view.seats.len();
    let you = view.you_seat.unwrap_or(0);

    commands.remove_resource::<Selection>();
    commands.insert_resource(RevealCount(0));

    // 把大厅 BGM 换成对局 BGM，并放一次发牌音效。
    for e in &music {
        commands.entity(e).despawn_recursive();
    }
    commands.spawn((AudioPlayer::new(audio.game_bgm.clone()), PlaybackSettings::LOOP, MpMusic));
    commands.spawn((AudioPlayer::new(audio.shuffle.clone()), PlaybackSettings::DESPAWN));

    // 背景（按场景）。
    commands.spawn((
        ImageNode::new(assets.load(format!("table/{}.png", view.scene_art))),
        Node { position_type: PositionType::Absolute, width: Val::Percent(100.0), height: Val::Percent(100.0), ..default() },
        GlobalZIndex(-1),
        SceneBackground,
    ));
    // 卡背。
    let card_back: Handle<Image> = assets.load(format!("cards/{}.png", pick(&CARD_BACKS)));
    commands.insert_resource(CardBack(card_back.clone()));

    // 状态栏。
    commands.spawn((
        Text::new(""),
        TextFont { font_size: 22.0, ..default() },
        TextColor(Color::srgb(0.95, 0.96, 0.98)),
        Node { position_type: PositionType::Absolute, top: Val::Px(12.0), left: Val::Px(16.0), max_width: Val::Px(360.0), ..default() },
        Slot::Status,
    ));

    // 公共池 5 张（可点选）。
    for i in 0..5 {
        let x = COMMUNITY_POOL_X + i as f32 * (COMMUNITY_CARD_W + COMMUNITY_CARD_GAP);
        let e = spawn_card(&mut commands, x, COMMUNITY_POOL_Y, COMMUNITY_CARD_W, COMMUNITY_CARD_H, COL_GOLD, card_back.clone(), Slot::Community(i));
        commands.entity(e).insert((Button, Interaction::default(), Selectable(i)));
        spawn_value_label(&mut commands, x, COMMUNITY_POOL_Y, COMMUNITY_CARD_W, COMMUNITY_CARD_H, Slot::Community(i));
    }
    spawn_label(&mut commands, COMMUNITY_POOL_X, COMMUNITY_POOL_Y - 28.0, "Community Pool");

    // 地牢 3 个。
    for i in 0..3 {
        let x = DUNGEON_POOL_X + i as f32 * (DUNGEON_CARD_W + DUNGEON_CARD_GAP);
        spawn_card(&mut commands, x, DUNGEON_POOL_Y, DUNGEON_CARD_W, DUNGEON_CARD_H, COL_RED, card_back.clone(), Slot::Dungeon(i));
        spawn_value_label(&mut commands, x, DUNGEON_POOL_Y, DUNGEON_CARD_W, DUNGEON_CARD_H, Slot::Dungeon(i));
    }
    spawn_label(&mut commands, DUNGEON_POOL_X, DUNGEON_POOL_Y - 28.0, "Dungeon / Boss");

    // 座位：当前客户端自己的座位固定在底部 dock，其它座位围绕牌桌。
    spawn_player_dock(&mut commands);
    for (seat, x, y) in seat_layout(count, you) {
        let local = seat == you;
        spawn_avatar(&mut commands, x, y, assets.load(mp_avatar_file(seat, you)));
        spawn_seat_name(
            &mut commands,
            if local { PLAYER_STATUS_X } else { x + SEAT_TEXT_X_OFFSET },
            if local { PLAYER_STATUS_Y } else { y },
            seat,
        );
        let (card_w, card_h, x0, cy) = if local {
            (PLAYER_HOLE_CARD_W, PLAYER_HOLE_CARD_H, PLAYER_HOLE_X, PLAYER_HOLE_Y)
        } else {
            (HOLE_CARD_W, HOLE_CARD_H, x, y + HOLE_CARD_TOP_OFFSET)
        };
        let x1 = x0 + card_w + HOLE_CARD_GAP;
        spawn_card(&mut commands, x0, cy, card_w, card_h, COL_HOLE, card_back.clone(), Slot::Hole { seat, idx: 0 });
        spawn_value_label(&mut commands, x0, cy, card_w, card_h, Slot::Hole { seat, idx: 0 });
        spawn_card(&mut commands, x1, cy, card_w, card_h, COL_HOLE, card_back.clone(), Slot::Hole { seat, idx: 1 });
        spawn_value_label(&mut commands, x1, cy, card_w, card_h, Slot::Hole { seat, idx: 1 });
    }

    // 借一锅按钮（默认隐藏，refresh 按 view.low_chips 显隐），放在 dock 上方。
    commands.spawn((
        Button,
        BorrowButton,
        Node {
            position_type: PositionType::Absolute,
            left: Val::Px(BORROW_BUTTON_X),
            top: Val::Px(BORROW_BUTTON_Y),
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

    spawn_bet_hud(&mut commands);
    spawn_pot_display(&mut commands);
    commands.insert_resource(PotState::default()); // 新局重置底池显示（含筹码堆重建标记）
}

/// 用最新视图刷新所有牌槽 + 借锅按钮显隐 + 背景。
fn mp_game_refresh(
    net: Option<Res<Net>>,
    back: Res<CardBack>,
    assets: Res<AssetServer>,
    selection: Option<Res<Selection>>,
    mut q: Query<(Option<&mut Text>, Option<&mut ImageNode>, Option<&mut BorderColor>, Option<&mut Card>, &Slot)>,
    mut bg_q: Query<&mut ImageNode, (With<SceneBackground>, Without<Slot>)>,
    mut borrow_q: Query<&mut Node, With<BorrowButton>>,
) {
    let Some(net) = net else { return };
    let Some(view) = &net.view else { return };
    let selecting = selection.is_some();
    let you = view.you_seat;

    for mut img in &mut bg_q {
        img.image = assets.load(format!("table/{}.png", view.scene_art));
    }
    for mut node in &mut borrow_q {
        node.display = if view.low_chips { Display::Flex } else { Display::None };
    }

    for (text, image, border, card_anim, slot) in &mut q {
        match slot {
            Slot::Status => set_text(text, mp_status_text(view)),
            Slot::SeatName(seat) => set_text(text, mp_seat_text(view, *seat)),
            Slot::Community(i) => {
                let face = view.community.get(*i);
                let (handle, col, val) = match face {
                    Some(c) => (assets.load(format!("cards/community/{}.png", c.art)), COL_GOLD, c.label.clone()),
                    None => (back.0.clone(), COL_DIM, String::new()),
                };
                set_image(image, handle);
                if !selecting {
                    set_border(border, col);
                }
                set_text(text, val);
                note_reveal(card_anim, face.is_some());
            }
            Slot::Dungeon(i) => {
                let face = view.dungeon.get(*i);
                let (handle, col, val) = match face {
                    Some(c) => (assets.load(format!("cards/dungeon/{}.png", c.art)), COL_RED, c.label.clone()),
                    None => (back.0.clone(), COL_DIM, String::new()),
                };
                set_image(image, handle);
                set_border(border, col);
                set_text(text, val);
                note_reveal(card_anim, face.is_some());
            }
            Slot::Hole { seat, idx } => {
                let seat = *seat;
                let idx = *idx;
                // 自己的底牌总亮；其余仅摊牌时亮（revealed_holes）。
                let card: Option<&CardView> = if Some(seat) == you {
                    view.your_hole.as_ref().map(|h| &h[idx])
                } else {
                    view.revealed_holes.get(seat).and_then(|o| o.as_ref()).map(|h| &h[idx])
                };
                let (handle, val) = match card {
                    Some(c) => (assets.load(format!("cards/community/{}.png", c.art)), c.label.clone()),
                    None => (back.0.clone(), String::new()),
                };
                let is_turn = view.seats.get(seat).map(|s| s.is_turn).unwrap_or(false);
                let col = if is_turn { COL_ACTIVE } else { COL_HOLE };
                set_image(image, handle);
                set_border(border, col);
                set_text(text, val);
                note_reveal(card_anim, card.is_some());
            }
        }
    }
}

fn mp_status_text(view: &GameView) -> String {
    let berserk = if view.boss_berserk { "  狂暴!" } else { "" };
    let mut s = format!(
        "联机 · 第 {} 手\n{}\nPot: {}   Bet: {}\n惊动 {:.1}{}\nP=战力 H=生命 T=威胁\n",
        view.hand_no + 1, view.scene_label, view.pot, view.current_bet, view.aggro, berserk,
    );
    match view.to_act_seat {
        Some(seat) => {
            let actor = view.seats.get(seat).map(|s| s.name.as_str()).unwrap_or("-");
            let yours = view.you_seat == Some(seat);
            if yours {
                s.push_str("轮到你：[Q]过 [W]跟 [E]加20 [R]弃 [T]全下");
            } else {
                s.push_str(&format!("等待 {actor} 行动…"));
            }
        }
        None => s.push_str("=== 摊牌结算 ==="),
    }
    s
}

fn mp_seat_text(view: &GameView, seat: usize) -> String {
    match view.seats.get(seat) {
        Some(s) => {
            let here = if s.is_turn { " *" } else { "" };
            let me = if view.you_seat == Some(seat) { " (你)" } else if s.is_ai { " (AI)" } else { "" };
            let debt = if s.debt > 0 { format!("\nDebt {}", s.debt) } else { String::new() };
            format!("{}{}{}\nChips {}\nin {} | {:?}{}", s.name, me, here, s.chips, s.committed, s.status, debt)
        }
        None => String::new(),
    }
}

/// 翻牌时（公共牌/地牢揭示数增加）放一次发牌音效。
fn mp_deal_sfx(
    mut commands: Commands,
    audio: Res<AudioAssets>,
    net: Option<Res<Net>>,
    reveal: Option<ResMut<RevealCount>>,
) {
    let (Some(net), Some(mut reveal)) = (net, reveal) else { return };
    let Some(view) = &net.view else { return };
    let revealed = view.community.len() + view.dungeon.len();
    if revealed > reveal.0 {
        commands.spawn((AudioPlayer::new(audio.shuffle.clone()), PlaybackSettings::DESPAWN));
    }
    reveal.0 = revealed;
}

/// 联机牌桌按钮：借一锅 / 下一手。
fn mp_game_buttons(
    client: Option<ResMut<RenetClient>>,
    mut borrow_q: Query<(&Interaction, &mut BackgroundColor), (Changed<Interaction>, With<BorrowButton>)>,
    mut next_q: Query<(&Interaction, &mut BackgroundColor), (Changed<Interaction>, With<NextHandButton>, Without<BorrowButton>)>,
) {
    let Some(mut client) = client else { return };
    for (interaction, mut bg) in &mut borrow_q {
        match interaction {
            Interaction::Pressed => {
                client.send_message(DefaultChannel::ReliableOrdered, ClientMsg::Borrow.encode());
                bg.0 = Color::srgb(0.32, 0.25, 0.14);
            }
            Interaction::Hovered => bg.0 = Color::srgb(0.26, 0.21, 0.13),
            Interaction::None => bg.0 = Color::srgb(0.20, 0.16, 0.10),
        }
    }
    for (interaction, mut bg) in &mut next_q {
        match interaction {
            Interaction::Pressed => {
                client.send_message(DefaultChannel::ReliableOrdered, ClientMsg::NextHand.encode());
            }
            Interaction::Hovered => bg.0 = BTN_HOVER,
            Interaction::None => bg.0 = BTN_NORMAL,
        }
    }
}

/// 确认手选 → 发给服务器（联机版；不在本地结算）。
fn mp_confirm(
    client: Option<ResMut<RenetClient>>,
    selection: Option<Res<Selection>>,
    q: Query<&Interaction, (Changed<Interaction>, With<ConfirmButton>)>,
) {
    let (Some(mut client), Some(sel)) = (client, selection) else { return };
    if sel.picks.len() != 3 {
        return;
    }
    for interaction in &q {
        if *interaction == Interaction::Pressed {
            let picks = [sel.picks[0], sel.picks[1], sel.picks[2]];
            client.send_message(DefaultChannel::ReliableOrdered, ClientMsg::SelectCards { picks }.encode());
        }
    }
}

/// 管理对局 overlay：结算面板 + 点选 UI 的出现/消失（据视图）。
fn mp_game_overlays(
    mut commands: Commands,
    net: Option<Res<Net>>,
    selection: Option<Res<Selection>>,
    result_ui: Query<Entity, With<ResultUi>>,
    select_ui: Query<Entity, With<SelectUi>>,
) {
    let Some(net) = net else { return };
    let Some(view) = &net.view else { return };

    let has_result = view.result.is_some();
    if has_result && result_ui.is_empty() {
        mp_spawn_result_panel(&mut commands, view);
    } else if !has_result && !result_ui.is_empty() {
        for e in &result_ui {
            commands.entity(e).despawn_recursive();
        }
    }

    if view.need_select && selection.is_none() {
        commands.insert_resource(Selection::default());
        spawn_select_ui(&mut commands);
    } else if !view.need_select && selection.is_some() {
        commands.remove_resource::<Selection>();
        for e in &select_ui {
            commands.entity(e).despawn_recursive();
        }
    }
}

/// 赢家文字：空=无人通关(滚存)；1 个=独赢；多个=战后优势值并列均分。
fn winners_line(winners: &[game_core::PlayerId], pot: u32, name_of: impl Fn(game_core::PlayerId) -> String) -> String {
    match winners.len() {
        0 => format!("无人通关，底池 {pot} 滚入下一手"),
        1 => format!("赢家：{}    底池 {pot}", name_of(winners[0])),
        n => {
            let names: Vec<String> = winners.iter().map(|&id| name_of(id)).collect();
            format!("平局均分：{}    底池 {pot} ÷ {n} = {}/人", names.join("、"), pot / n as u32)
        }
    }
}

/// 联机结算计分板（从 GameView 构建；只有房主显示「下一手」）。
fn mp_spawn_result_panel(commands: &mut Commands, view: &GameView) {
    let Some(settle) = &view.result else { return };
    let total_t: u32 = view.dungeon_threats.iter().sum();
    let total_h: u32 = 0; // 节点生命已在牌面显示，这里只汇总威胁
    let _ = total_h;
    let mut dungeon_line = format!("地牢威胁合计 {}   惊动 {:.1}", total_t, view.aggro);
    if view.boss_berserk {
        dungeon_line.push_str("  狂暴!");
    }

    let name_of = |id: game_core::PlayerId| view.seats.get(id.0 as usize).map(|s| s.name.clone()).unwrap_or_else(|| "?".into());
    let mut board = String::new();
    for r in &settle.results {
        let nm = name_of(r.id);
        if r.cleared {
            board.push_str(&format!(
                "{:<11} 队伍 P{:<3} H{:<3} →  通关  余血{} 战力{}  优势 {}\n",
                nm, r.team_power, r.team_health, r.remaining_health, r.remaining_power, r.advantage
            ));
        } else {
            board.push_str(&format!("{:<11} 队伍 P{:<3} H{:<3} →  团灭\n", nm, r.team_power, r.team_health));
        }
    }
    let winner_line = winners_line(&settle.winners, view.pot, name_of);
    let can_next = view.can_next;

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
                    Text::new(format!("本手结算 · {}", view.scene_label)),
                    TextFont { font_size: 38.0, ..default() },
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
                    TextFont { font_size: 26.0, ..default() },
                    TextColor(Color::srgb(0.95, 0.85, 0.55)),
                ));
                if can_next {
                    panel
                        .spawn((
                            Button,
                            NextHandButton,
                            Node {
                                width: Val::Px(280.0),
                                height: Val::Px(60.0),
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
                                TextFont { font_size: 26.0, ..default() },
                                TextColor(Color::srgb(0.92, 0.94, 0.97)),
                            ));
                        });
                } else {
                    panel.spawn((
                        Text::new("等待房主开始下一手…"),
                        TextFont { font_size: 20.0, ..default() },
                        TextColor(Color::srgb(0.6, 0.63, 0.7)),
                    ));
                }
            });
        });
}

// ====================== 下注 HUD（底部按钮 + 加注拉杆 + 特效）======================

const RAISE_STEP: u32 = 20;
const SLIDER_W: f32 = 300.0; // 拉杆轨道宽
const BET_COL_CHECK: Color = Color::srgb(0.24, 0.40, 0.58);
const BET_COL_CALL: Color = Color::srgb(0.20, 0.52, 0.30);
const BET_COL_FOLD: Color = Color::srgb(0.58, 0.26, 0.26);
const BET_COL_RAISE: Color = Color::srgb(0.62, 0.50, 0.22);
const BET_COL_DISABLED: Color = Color::srgb(0.16, 0.17, 0.21);

/// 四个下注动作（按钮种类 + 特效颜色来源）。
#[derive(Component, Clone, Copy, PartialEq, Eq)]
enum BetButton {
    Check,
    Call,
    Fold,
    Raise,
}

/// 下注 HUD 根（轮到自己时显示）。
#[derive(Component)]
struct BetHud;
/// 加注拉杆轨道（可点/拖）。
#[derive(Component)]
struct RaiseTrack;
/// 加注拉杆的滑块。
#[derive(Component)]
struct RaiseHandle;
/// 按钮上需要随金额刷新的文字。
#[derive(Component)]
struct BetLabel(BetButton);
/// 飘字特效（上浮 + 淡出后销毁）。
#[derive(Component)]
struct Floater {
    timer: Timer,
}

/// 下注上下文：从单机 Match 或联机 GameView 每帧填充，HUD/键盘据此判活与算金额。
#[derive(Resource, Default)]
struct Bet {
    active: bool,    // 是否轮到我
    current_bet: u32,
    committed: u32,
    chips: u32,
    can_check: bool,
    raise_to: u32,   // 拉杆当前的加注总额（默认 = 最小加注）
    min_raise: u32,
    max_raise: u32,  // = committed + chips（拉到顶即全下）
}

impl Bet {
    fn need(&self) -> u32 {
        self.current_bet.saturating_sub(self.committed)
    }
    fn can_raise(&self) -> bool {
        self.max_raise > self.current_bet && self.chips > self.need()
    }
    /// 重新计算 min/max 并把 raise_to 夹回区间；turn_start 时重置到最小加注。
    fn recompute(&mut self, turn_start: bool) {
        self.max_raise = self.committed + self.chips;
        self.min_raise = (self.current_bet + RAISE_STEP).min(self.max_raise);
        if turn_start || self.raise_to < self.min_raise || self.raise_to > self.max_raise {
            self.raise_to = self.min_raise;
        }
    }
}

fn bet_color(kind: BetButton) -> Color {
    match kind {
        BetButton::Check => BET_COL_CHECK,
        BetButton::Call => BET_COL_CALL,
        BetButton::Fold => BET_COL_FOLD,
        BetButton::Raise => BET_COL_RAISE,
    }
}

/// 玩家想执行的下注动作（按钮/键盘发，SP 本地执行 / MP 发服务器 / 特效消费）。
#[derive(Event)]
struct BetIntent {
    action: Action,
    kind: BetButton,
}

/// 铺底部下注 HUD（默认隐藏；bet_hud_update 按是否轮到自己显隐）。单机和联机都调用。
fn spawn_bet_hud(commands: &mut Commands) {
    commands
        .spawn((
            BetHud,
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(PLAYER_ACTION_X),
                top: Val::Px(PLAYER_ACTION_Y),
                width: Val::Px(PLAYER_ACTION_W),
                height: Val::Px(PLAYER_ACTION_H),
                flex_direction: FlexDirection::Row,
                column_gap: Val::Px(14.0),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                display: Display::None,
                ..default()
            },
            GlobalZIndex(45),
        ))
        .with_children(|hud| {
            bet_btn(hud, BetButton::Check, "过牌");
            bet_btn(hud, BetButton::Call, "跟注");
            bet_btn(hud, BetButton::Fold, "弃牌");
            // 加注：拉杆 + 按钮一列。
            hud.spawn(Node {
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(6.0),
                align_items: AlignItems::Center,
                ..default()
            })
            .with_children(|col| {
                // 拉杆轨道（内含滑块）。挂 RelativeCursorPosition 供 bet_slider_drag
                // 拿到归一化光标位置（自动处理 DPI 缩放，避免拖不动）。
                col.spawn((
                    Button,
                    RaiseTrack,
                    RelativeCursorPosition::default(),
                    Node {
                        width: Val::Px(SLIDER_W),
                        height: Val::Px(28.0),
                        border: UiRect::all(Val::Px(2.0)),
                        ..default()
                    },
                    BorderColor(Color::srgb(0.5, 0.45, 0.3)),
                    BackgroundColor(Color::srgb(0.14, 0.13, 0.10)),
                ))
                .with_children(|track| {
                    track.spawn((
                        RaiseHandle,
                        Node {
                            position_type: PositionType::Absolute,
                            left: Val::Px(0.0),
                            top: Val::Px(-5.0),
                            width: Val::Px(16.0),
                            height: Val::Px(30.0),
                            ..default()
                        },
                        BackgroundColor(Color::srgb(0.95, 0.82, 0.4)),
                    ));
                });
                col.spawn((
                    Button,
                    BetButton::Raise,
                    Node {
                        width: Val::Px(SLIDER_W),
                        height: Val::Px(56.0),
                        justify_content: JustifyContent::Center,
                        align_items: AlignItems::Center,
                        border: UiRect::all(Val::Px(2.0)),
                        ..default()
                    },
                    BorderColor(Color::srgb(0.45, 0.48, 0.58)),
                    BackgroundColor(BET_COL_RAISE),
                ))
                .with_children(|b| {
                    b.spawn((
                        Text::new("加注"),
                        TextFont { font_size: 24.0, ..default() },
                        TextColor(Color::srgb(0.96, 0.96, 0.98)),
                        TextLayout::new_with_justify(JustifyText::Center),
                        BetLabel(BetButton::Raise),
                    ));
                });
            });
        });
}

fn bet_btn(parent: &mut ChildBuilder, kind: BetButton, label: &str) {
    parent
        .spawn((
            Button,
            kind,
            Node {
                width: Val::Px(146.0),
                height: Val::Px(84.0),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                border: UiRect::all(Val::Px(2.0)),
                ..default()
            },
            BorderColor(Color::srgb(0.45, 0.48, 0.58)),
            BackgroundColor(bet_color(kind)),
        ))
        .with_children(|b| {
            b.spawn((
                Text::new(label),
                TextFont { font_size: 26.0, ..default() },
                TextColor(Color::srgb(0.96, 0.96, 0.98)),
                TextLayout::new_with_justify(JustifyText::Center),
                BetLabel(kind),
            ));
        });
}

/// 单机：从本地 Match 填下注上下文。
fn bet_ctx_sp(session: Option<Res<GameSession>>, mut bet: ResMut<Bet>) {
    let Some(session) = session else {
        bet.active = false;
        return;
    };
    let g = &session.mtch.hand;
    let was = bet.active;
    let me_turn = !matches!(g.phase, Phase::Showdown | Phase::Done)
        && g.players.get(g.to_act).map(|p| !p.is_ai).unwrap_or(false);
    if !me_turn {
        bet.active = false;
        return;
    }
    let p = &g.players[g.to_act];
    bet.active = true;
    bet.current_bet = g.current_bet;
    bet.committed = p.committed;
    bet.chips = p.chips;
    bet.can_check = p.committed >= g.current_bet;
    bet.recompute(!was);
}

/// 联机：从服务器视图填下注上下文。
fn bet_ctx_mp(net: Option<Res<Net>>, mut bet: ResMut<Bet>) {
    let Some(net) = net else {
        bet.active = false;
        return;
    };
    let Some(view) = &net.view else {
        bet.active = false;
        return;
    };
    let was = bet.active;
    let me_turn = view.to_act_seat.is_some() && view.to_act_seat == view.you_seat;
    let seat = view.you_seat.and_then(|i| view.seats.get(i));
    match (me_turn, seat) {
        (true, Some(s)) => {
            bet.active = true;
            bet.current_bet = view.current_bet;
            bet.committed = s.committed;
            bet.chips = s.chips;
            bet.can_check = s.committed >= view.current_bet;
            bet.recompute(!was);
        }
        _ => bet.active = false,
    }
}

/// 显隐 HUD、刷新按钮金额文字、置灰不可用项、摆拉杆滑块。
fn bet_hud_update(
    bet: Res<Bet>,
    mut hud: Query<&mut Node, (With<BetHud>, Without<RaiseHandle>)>,
    mut handle: Query<&mut Node, (With<RaiseHandle>, Without<BetHud>)>,
    mut labels: Query<(&mut Text, &BetLabel)>,
    mut btns: Query<(&BetButton, &mut BackgroundColor)>,
) {
    for mut n in &mut hud {
        n.display = if bet.active { Display::Flex } else { Display::None };
    }
    if !bet.active {
        return;
    }
    // 滑块位置。
    let span = bet.max_raise.saturating_sub(bet.min_raise);
    let frac = if span > 0 {
        (bet.raise_to.saturating_sub(bet.min_raise)) as f32 / span as f32
    } else {
        0.0
    };
    for mut n in &mut handle {
        n.left = Val::Px(frac * (SLIDER_W - 16.0));
    }
    // 按钮文字。
    for (mut t, lbl) in &mut labels {
        match lbl.0 {
            BetButton::Call => t.0 = if bet.need() > 0 { format!("跟注 {}", bet.need()) } else { "跟注".into() },
            BetButton::Raise => {
                t.0 = if bet.raise_to >= bet.max_raise {
                    format!("全下 {}", bet.max_raise)
                } else {
                    format!("加注到 {}", bet.raise_to)
                }
            }
            _ => {}
        }
    }
    // 置灰不可用按钮。
    for (kind, mut bg) in &mut btns {
        let ok = match kind {
            BetButton::Check => bet.can_check,
            BetButton::Call => bet.need() > 0,
            BetButton::Fold => true,
            BetButton::Raise => bet.can_raise(),
        };
        bg.0 = if ok { bet_color(*kind) } else { BET_COL_DISABLED };
    }
}

/// 把一次下注意图转成动作（含「拉满=全下」「不能过牌就忽略」等）。
fn intent_for(kind: BetButton, bet: &Bet) -> Option<Action> {
    match kind {
        BetButton::Check => bet.can_check.then_some(Action::Check),
        BetButton::Call => Some(Action::Call),
        BetButton::Fold => Some(Action::Fold),
        BetButton::Raise => {
            if !bet.can_raise() {
                None
            } else if bet.raise_to >= bet.max_raise {
                Some(Action::AllIn)
            } else {
                Some(Action::Raise { to: bet.raise_to })
            }
        }
    }
}

/// 点按钮 → 发下注意图。
fn bet_hud_buttons(
    bet: Res<Bet>,
    mut ev: EventWriter<BetIntent>,
    q: Query<(&Interaction, &BetButton), Changed<Interaction>>,
) {
    if !bet.active {
        return;
    }
    for (interaction, kind) in &q {
        if *interaction == Interaction::Pressed {
            if let Some(action) = intent_for(*kind, &bet) {
                ev.send(BetIntent { action, kind: *kind });
            }
        }
    }
}

/// 键盘下注：Q过 W跟 E加注 R弃 T全下（与按钮等效，统一走意图）。
fn bet_keys(keys: Res<ButtonInput<KeyCode>>, bet: Res<Bet>, mut ev: EventWriter<BetIntent>) {
    if !bet.active {
        return;
    }
    let intent = if keys.just_pressed(KeyCode::KeyQ) {
        intent_for(BetButton::Check, &bet).map(|a| (a, BetButton::Check))
    } else if keys.just_pressed(KeyCode::KeyW) {
        Some((Action::Call, BetButton::Call))
    } else if keys.just_pressed(KeyCode::KeyE) {
        intent_for(BetButton::Raise, &bet).map(|a| (a, BetButton::Raise))
    } else if keys.just_pressed(KeyCode::KeyR) {
        Some((Action::Fold, BetButton::Fold))
    } else if keys.just_pressed(KeyCode::KeyT) {
        bet.can_raise().then_some((Action::AllIn, BetButton::Raise))
    } else {
        None
    };
    if let Some((action, kind)) = intent {
        ev.send(BetIntent { action, kind });
    }
}

/// 加注额控制：拖动拉杆（点哪跳哪、按住可拖）+ 滚轮微调。
///
/// 用 `RelativeCursorPosition`（Bevy 内部已处理 DPI 缩放 / 视口换算）拿轨道内的归一化
/// 光标位置，避免「UI 的 GlobalTransform 是物理像素、cursor_position 是逻辑像素」在
/// 缩放显示器上对不上导致的「拖不动」。单点拉杆 = 跳到该位置；按住拖 = 连续设额；
/// 滚轮 = 每格 ±RAISE_STEP，作为稳妥的补充手段。
fn bet_slider_drag(
    mouse: Res<ButtonInput<MouseButton>>,
    mut wheel: EventReader<MouseWheel>,
    mut bet: ResMut<Bet>,
    track: Query<&RelativeCursorPosition, With<RaiseTrack>>,
    mut dragging: Local<bool>,
) {
    if !bet.active || bet.max_raise <= bet.min_raise {
        *dragging = false;
        wheel.clear();
        return;
    }
    let span = (bet.max_raise - bet.min_raise) as f32;
    if let Ok(rel) = track.get_single() {
        if !mouse.pressed(MouseButton::Left) {
            *dragging = false;
        }
        if mouse.just_pressed(MouseButton::Left) && rel.mouse_over() {
            *dragging = true;
        }
        if *dragging {
            if let Some(n) = rel.normalized {
                let frac = n.x.clamp(0.0, 1.0);
                bet.raise_to = bet.min_raise + (frac * span).round() as u32;
            }
        }
    }
    // 滚轮微调（轮到自己时随时可用）。
    let mut steps = 0i32;
    for ev in wheel.read() {
        steps += ev.y.signum() as i32;
    }
    if steps != 0 {
        let v = bet.raise_to as i32 + steps * RAISE_STEP as i32;
        bet.raise_to = v.clamp(bet.min_raise as i32, bet.max_raise as i32) as u32;
    }
}

/// 下注按钮悬停放大 + 边框高亮（用 Transform 缩放，不触发 flex 重排）。
fn bet_button_hover(
    time: Res<Time>,
    bet: Res<Bet>,
    mut q: Query<(&Interaction, &BetButton, &mut Transform, &mut BorderColor)>,
) {
    let k = 1.0 - (-time.delta_secs() * 16.0).exp();
    let base = Color::srgb(0.45, 0.48, 0.58);
    let bright = Color::srgb(0.95, 0.85, 0.45);
    for (interaction, kind, mut tf, mut border) in &mut q {
        let enabled = match kind {
            BetButton::Check => bet.can_check,
            BetButton::Call => bet.need() > 0,
            BetButton::Fold => true,
            BetButton::Raise => bet.can_raise(),
        };
        let hovered = bet.active && enabled && matches!(interaction, Interaction::Hovered | Interaction::Pressed);
        let target = if hovered { 1.12 } else { 1.0 };
        let next = tf.scale.x + (target - tf.scale.x) * k;
        tf.scale = Vec3::new(next, next, 1.0);
        let t = ((next - 1.0) / 0.12).clamp(0.0, 1.0);
        border.0 = mix_color(base, bright, t);
    }
}

/// 两色线性插值（用于悬停高亮渐变）。
fn mix_color(a: Color, b: Color, t: f32) -> Color {
    let a = a.to_srgba();
    let b = b.to_srgba();
    Color::srgb(
        a.red + (b.red - a.red) * t,
        a.green + (b.green - a.green) * t,
        a.blue + (b.blue - a.blue) * t,
    )
}

/// 单机：执行下注意图。
fn apply_bet_sp(mut ev: EventReader<BetIntent>, mut session: ResMut<GameSession>) {
    for intent in ev.read() {
        let g = &mut session.mtch.hand;
        if matches!(g.phase, Phase::Showdown | Phase::Done) || g.players[g.to_act].is_ai {
            continue;
        }
        let _ = g.apply(intent.action);
    }
}

/// 联机：把下注意图发给服务器。
fn send_bet_mp(mut ev: EventReader<BetIntent>, client: Option<ResMut<RenetClient>>, net: Option<Res<Net>>) {
    let (Some(mut client), Some(net)) = (client, net) else {
        ev.clear();
        return;
    };
    let your_turn = net.view.as_ref().map(|v| v.to_act_seat.is_some() && v.to_act_seat == v.you_seat).unwrap_or(false);
    for intent in ev.read() {
        if your_turn {
            client.send_message(DefaultChannel::ReliableOrdered, ClientMsg::Act { action: intent.action }.encode());
        }
    }
}

/// 特效：每次下注意图，在屏幕中下方冒一个带色飘字。
fn bet_effects(mut commands: Commands, mut ev: EventReader<BetIntent>) {
    for intent in ev.read() {
        let (txt, col) = match intent.kind {
            BetButton::Check => ("过牌", BET_COL_CHECK),
            BetButton::Call => ("跟注!", BET_COL_CALL),
            BetButton::Fold => ("弃牌", BET_COL_FOLD),
            BetButton::Raise => {
                if intent.action == Action::AllIn {
                    ("全下!!", Color::srgb(0.95, 0.4, 0.3))
                } else {
                    ("加注!", BET_COL_RAISE)
                }
            }
        };
        commands.spawn((
            Floater { timer: Timer::from_seconds(0.9, TimerMode::Once) },
            Text::new(txt),
            TextFont { font_size: 64.0, ..default() },
            TextColor(col),
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(940.0),
                top: Val::Px(840.0),
                ..default()
            },
            GlobalZIndex(80),
        ));
    }
}

/// 飘字动画：上浮 + 淡出，结束销毁。
fn floaters(mut commands: Commands, time: Res<Time>, mut q: Query<(Entity, &mut Floater, &mut Node, &mut TextColor)>) {
    for (e, mut f, mut node, mut color) in &mut q {
        f.timer.tick(time.delta());
        if let Val::Px(t) = node.top {
            node.top = Val::Px(t - 70.0 * time.delta_secs());
        }
        color.0.set_alpha(1.0 - f.timer.fraction());
        if f.timer.finished() {
            commands.entity(e).despawn_recursive();
        }
    }
}

// ============================ 牌面动画 + 底池 ============================

/// 牌位动画：悬停弹出（抬升 + 放大 + 描边光晕 + 置顶）与发牌/翻牌弹跳。
/// 全程跑，没有 Card 组件的实体不受影响。
fn animate_cards(
    time: Res<Time>,
    mut q: Query<(&Interaction, &mut Card, &mut Node, &mut GlobalZIndex, &mut Outline)>,
) {
    let dt = time.delta_secs();
    let k = 1.0 - (-dt * 16.0).exp(); // 指数平滑系数（帧率无关）
    for (interaction, mut card, mut node, mut z, mut outline) in &mut q {
        let target = if matches!(interaction, Interaction::Hovered | Interaction::Pressed) { 1.0 } else { 0.0 };
        card.lift += (target - card.lift) * k;
        if card.deal > 0.0 {
            card.deal = (card.deal - dt * 4.0).max(0.0); // 约 0.25s
        }
        let lift = card.lift;
        // 发牌弹跳：从放大 +0.30 收回到 0（平方让收尾更柔和）。
        let punch = 0.30 * card.deal * card.deal;
        let scale = 1.0 + 0.12 * lift + punch;
        let w = card.w * scale;
        let h = card.h * scale;
        node.width = Val::Px(w);
        node.height = Val::Px(h);
        node.left = Val::Px(card.left - (w - card.w) / 2.0);
        node.top = Val::Px(card.top - (h - card.h) / 2.0 - 18.0 * lift);
        z.0 = if lift > 0.02 || card.deal > 0.0 { 50 } else { 0 };
        outline.width = Val::Px(3.5 * lift);
        outline.color = COL_SELECT.with_alpha(lift);
    }
}

/// 牌桌中央的底池面板：装饰筹码堆 + 实时金额。单机/联机进局时各调一次。
fn spawn_pot_display(commands: &mut Commands) {
    commands
        .spawn((
            PotDisplayRoot,
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(POT_X),
                top: Val::Px(POT_Y),
                width: Val::Px(POT_W),
                height: Val::Px(POT_H),
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                column_gap: Val::Px(16.0),
                border: UiRect::all(Val::Px(2.0)),
                ..default()
            },
            BorderColor(Color::srgba(0.85, 0.72, 0.35, 0.55)),
            BackgroundColor(Color::srgba(0.05, 0.045, 0.03, 0.72)),
            BorderRadius::all(Val::Px(16.0)),
            GlobalZIndex(40),
        ))
        .with_children(|p| {
            // 筹码堆容器（枚数随底池增多，由 pot_display 重建）。
            p.spawn((
                Node {
                    width: Val::Px(86.0),
                    height: Val::Px(70.0),
                    ..default()
                },
                PotChipStack,
            ));
            p.spawn((
                Text::new("底池 0"),
                TextFont { font_size: 34.0, ..default() },
                TextColor(Color::srgb(0.97, 0.86, 0.50)),
                PotAmountText,
            ));
        });
}

/// 底池筹码枚数：每 100 一枚，封顶 18。底池为 0 时不显示。
fn chip_count(pot: u32) -> usize {
    if pot == 0 {
        0
    } else {
        ((pot / 100) as usize + 1).min(18)
    }
}

/// 按枚数重建筹码堆：分列（每列 6 枚）向上叠放，像扑克筹码堆。
fn rebuild_chip_stack(commands: &mut Commands, stack: Entity, n: usize) {
    commands.entity(stack).despawn_descendants();
    commands.entity(stack).with_children(|p| {
        for i in 0..n {
            let col = (i / 6) as f32;
            let row = (i % 6) as f32;
            let color = match i % 4 {
                0 => Color::srgb(0.78, 0.22, 0.24),
                1 => Color::srgb(0.22, 0.40, 0.72),
                2 => Color::srgb(0.24, 0.58, 0.34),
                _ => Color::srgb(0.90, 0.74, 0.30),
            };
            p.spawn((
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Px(col * 18.0),
                    top: Val::Px(40.0 - row * 8.0),
                    width: Val::Px(CHIP_D),
                    height: Val::Px(CHIP_D),
                    border: UiRect::all(Val::Px(3.0)),
                    ..default()
                },
                BorderColor(Color::srgba(1.0, 1.0, 1.0, 0.65)),
                BackgroundColor(color),
                BorderRadius::all(Val::Px(CHIP_D / 2.0)),
            ));
        }
    });
}

/// 单机：每帧把本地底池写进 PotState。
fn update_pot_sp(session: Option<Res<GameSession>>, mut pot: ResMut<PotState>) {
    if let Some(s) = session {
        pot.target = s.mtch.hand.pot;
    }
}

/// 联机：每帧把服务器视图的底池写进 PotState。
fn update_pot_mp(net: Option<Res<Net>>, mut pot: ResMut<PotState>) {
    if let Some(net) = net {
        if let Some(view) = &net.view {
            pot.target = view.pot;
        }
    }
}

/// 刷新底池数字（平滑滚动）；涨钱时脉冲高亮并撒筹码落入底池。
fn pot_display(
    mut commands: Commands,
    time: Res<Time>,
    mut pot: ResMut<PotState>,
    mut text_q: Query<(&mut Text, &mut TextFont), With<PotAmountText>>,
    mut root_q: Query<&mut BorderColor, With<PotDisplayRoot>>,
    stack_q: Query<Entity, With<PotChipStack>>,
) {
    if pot.target != pot.last_target {
        if pot.target > pot.last_target {
            pot.flash = 1.0;
            spawn_pot_chips(&mut commands);
        } else {
            // 新一手底池清零：直接归位，别从大数往下滚。
            pot.shown = pot.target as f32;
        }
        pot.last_target = pot.target;
    }
    // 筹码堆枚数随底池变化时重建。
    let tier = chip_count(pot.target);
    if pot.chip_tier != Some(tier) {
        pot.chip_tier = Some(tier);
        if let Ok(stack) = stack_q.get_single() {
            rebuild_chip_stack(&mut commands, stack, tier);
        }
    }
    let dt = time.delta_secs();
    pot.shown += (pot.target as f32 - pot.shown) * (1.0 - (-dt * 9.0).exp());
    if (pot.shown - pot.target as f32).abs() < 0.5 {
        pot.shown = pot.target as f32;
    }
    pot.flash = (pot.flash - dt * 1.8).max(0.0);
    let flash = pot.flash;
    for (mut t, mut font) in &mut text_q {
        t.0 = format!("底池 {}", pot.shown.round() as u32);
        font.font_size = 34.0 + 12.0 * flash;
    }
    for mut bc in &mut root_q {
        bc.0 = Color::srgba(
            (0.85 + 0.15 * flash).min(1.0),
            (0.72 + 0.22 * flash).min(1.0),
            0.35 + 0.40 * flash,
            (0.55 + 0.45 * flash).min(1.0),
        );
    }
}

/// 撒一把筹码从上方落入底池（涨钱时调用）。
fn spawn_pot_chips(commands: &mut Commands) {
    let cx = POT_X + POT_W / 2.0;
    let mut rng = game_core::rng::Rng::new(random_seed());
    for _ in 0..5 {
        let off = (rng.next_u64() % 140) as f32 - 70.0;
        let col = match rng.next_u64() % 4 {
            0 => Color::srgb(0.78, 0.22, 0.24),
            1 => Color::srgb(0.22, 0.40, 0.72),
            2 => Color::srgb(0.24, 0.58, 0.34),
            _ => Color::srgb(0.90, 0.74, 0.30),
        };
        let left = cx + off - CHIP_D / 2.0;
        let from_top = POT_Y - 130.0 - (rng.next_u64() % 50) as f32;
        let to_top = POT_Y + 4.0 + (rng.next_u64() % 34) as f32;
        commands.spawn((
            PotChip { timer: Timer::from_seconds(0.55, TimerMode::Once), from_top, to_top },
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(left),
                top: Val::Px(from_top),
                width: Val::Px(CHIP_D),
                height: Val::Px(CHIP_D),
                border: UiRect::all(Val::Px(3.0)),
                ..default()
            },
            BorderColor(Color::srgba(1.0, 1.0, 1.0, 0.7)),
            BackgroundColor(col),
            BorderRadius::all(Val::Px(CHIP_D / 2.0)),
            GlobalZIndex(60),
        ));
    }
}

/// 筹码下落动画：加速落入底池，尾段淡出后销毁。
fn chip_drops(
    mut commands: Commands,
    time: Res<Time>,
    mut q: Query<(Entity, &mut PotChip, &mut Node, &mut BackgroundColor, &mut BorderColor)>,
) {
    for (e, mut chip, mut node, mut bg, mut border) in &mut q {
        chip.timer.tick(time.delta());
        let f = chip.timer.fraction();
        let ease = 1.0 - (1.0 - f) * (1.0 - f); // ease-out（落下加速感）
        node.top = Val::Px(chip.from_top + (chip.to_top - chip.from_top) * ease);
        let a = if f > 0.7 { (1.0 - (f - 0.7) / 0.3).max(0.0) } else { 1.0 };
        bg.0.set_alpha(a);
        border.0.set_alpha(a * 0.7);
        if chip.timer.finished() {
            commands.entity(e).despawn_recursive();
        }
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
                left: Val::Px(PLAYER_ACTION_X),
                top: Val::Px(BORROW_BUTTON_Y),
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

    let name_of = |id: game_core::PlayerId| {
        m.seats.iter().find(|s| s.id == id).map(|s| s.name.clone()).unwrap_or_else(|| "?".into())
    };
    let winner_line = winners_line(&settle.winners, g.pot, name_of);

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
