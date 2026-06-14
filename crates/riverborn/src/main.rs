//! RiverBorn —— Bevy 表现层（单机对 AI 起步骨架）。
//!
//! 现在做的事：把 game_core 的一局塞进 Bevy 资源，渲染一个最小的
//! HUD（当前阶段 / 底池 / 公共牌 / 地牢），并用键盘驱动下注动作。
//! 真正的卡牌美术、动画、AI 决策后续再加 —— 规则全在 game_core，
//! 这一层只负责「显示状态 + 收集输入」。

use bevy::prelude::*;
use game_core::{Action, Adventurer, Class, Game, Phase, Player};

/// 把整局游戏作为一个 Bevy 资源持有。
#[derive(Resource)]
struct GameSession {
    game: Game,
}

#[derive(Component)]
struct HudText;

fn main() {
    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "RiverBorn".into(),
                resolution: (900., 600.).into(),
                ..default()
            }),
            ..default()
        }))
        .insert_resource(ClearColor(Color::srgb(0.08, 0.09, 0.12)))
        .insert_resource(new_session())
        .add_systems(Startup, setup_ui)
        .add_systems(Update, (ai_auto_play, handle_input, refresh_hud).chain())
        .run();
}

fn new_session() -> GameSession {
    let players = vec![
        Player::new(
            0,
            "You",
            false,
            200,
            [
                Adventurer::new(Class::Warrior, 4, 9),
                Adventurer::new(Class::Cleric, 2, 6),
            ],
        ),
        Player::new(
            1,
            "AI-Gambler",
            true,
            200,
            [
                Adventurer::new(Class::Mage, 6, 3),
                Adventurer::new(Class::Rogue, 5, 4),
            ],
        ),
    ];
    // seed 用 "RIVE" 的 ASCII，纯粹图个固定起手；正式版可换成时间/随机源。
    GameSession {
        game: Game::new(players, 10, 0x5249_5645),
    }
}

fn setup_ui(mut commands: Commands) {
    commands.spawn(Camera2d);
    commands.spawn((
        Text::new(""),
        TextFont { font_size: 22.0, ..default() },
        TextColor(Color::srgb(0.9, 0.92, 0.95)),
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(24.0),
            left: Val::Px(24.0),
            ..default()
        },
        HudText,
    ));
}

/// AI 玩家自动行动，用一个计时器拉开节奏，方便人类玩家旁观。
fn ai_auto_play(time: Res<Time>, mut delay: Local<f32>, mut session: ResMut<GameSession>) {
    let g = &mut session.game;
    if g.phase == Phase::Showdown || g.phase == Phase::Done {
        return;
    }
    let idx = g.to_act;
    if !g.players[idx].is_ai {
        *delay = 0.0; // 轮到人类，重置计时
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
        return; // 不是人类回合，忽略按键
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
        // 失败（比如不能 check）就忽略，等下一次合法输入。
        let _ = g.apply(a);
    }
}

fn refresh_hud(session: Res<GameSession>, mut q: Query<&mut Text, With<HudText>>) {
    let g = &session.game;
    let mut text = match q.get_single_mut() {
        Ok(t) => t,
        Err(_) => return,
    };

    let mut s = String::new();
    s.push_str(&format!("Phase: {:?}\nPot: {}\nBet line: {}\n\n", g.phase, g.pot, g.current_bet));
    for (i, p) in g.players.iter().enumerate() {
        let here = if i == g.to_act && g.phase != Phase::Showdown { " <-- to act" } else { "" };
        s.push_str(&format!("[{}] {} chips:{} in:{} {:?}{}\n", i, p.name, p.chips, p.committed, p.status, here));
    }
    s.push_str(&format!("\nCommunity ({}): {:?}\n", g.community.len(), g.community));
    s.push_str(&format!("Dungeon   ({}): {:?}\n", g.dungeon.len(), g.dungeon));

    if g.phase == Phase::Showdown {
        let res = g.settle();
        s.push_str(&format!("\n=== SHOWDOWN ===\nWinner: {:?}\n{:?}\n", res.winner, res.results));
    } else {
        s.push_str("\n[Q]Check [W]Call [E]Raise+20 [R]Fold\n");
    }

    text.0 = s;
}
