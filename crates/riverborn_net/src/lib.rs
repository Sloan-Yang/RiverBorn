//! RiverBorn 联机协议：客户端与服务器之间的消息类型 + 序列化。
//!
//! 纯数据 + serde（JSON），不依赖 renet / bevy，两端共用。
//! 阶段 1 只覆盖「大厅」：连接、改名、建/搜/进/退房间、开始游戏。
//! 阶段 2 再加对局相关消息（玩家操作 + 裁剪过隐藏信息的 `GameView`）。

use game_core::{Action, Phase, PlayerStatus, Settlement};
use serde::{Deserialize, Serialize};

/// renet netcode 用的协议 ID（两端必须一致）。改了协议就改它，挡掉版本不匹配的连接。
pub const PROTOCOL_ID: u64 = 0x5249_5645_424F_524E; // "RIVEBORN"
/// 服务器默认 UDP 端口。
pub const DEFAULT_PORT: u16 = 7777;

/// 玩家在服务器上的唯一标识（= renet 的 client id）。
pub type PlayerId = u64;

/// 客户端 → 服务器。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum ClientMsg {
    /// 连上后第一件事：报上自己的名字。
    Hello { name: String },
    /// 建房：房名、人数上限(3~6)、是否用 AI 填满空位。
    CreateRoom { room_name: String, max_players: usize, fill_ai: bool },
    /// 拉取当前公开房间列表（搜索房间）。
    ListRooms,
    /// 用房间号加入。
    JoinRoom { code: String },
    /// 离开当前房间，回到大厅。
    LeaveRoom,
    /// 房主开始游戏。
    StartGame,
    // ---- 对局中 ----
    /// 下注操作（轮到自己时）。
    Act { action: Action },
    /// 摊牌时手选 3 张公共牌组队。
    SelectCards { picks: [usize; 3] },
    /// 借一锅。
    Borrow,
    /// 房主推进到下一手。
    NextHand,
}

/// 服务器 → 客户端。
#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum ServerMsg {
    /// 连接已确认，告知你的 id。
    Welcome { your_id: PlayerId },
    /// 房间列表（回应 ListRooms）。
    RoomList { rooms: Vec<RoomInfo> },
    /// 成功加入/创建房间，附上房间当前状态。
    Joined { room: RoomState },
    /// 房间状态变化（有人进/出、房主开始等）时广播给房内成员。
    RoomUpdate { room: RoomState },
    /// 你已离开房间。
    Left,
    /// 操作出错（房间满了 / 房间号不存在 / 非房主开始等）。
    Error { text: String },
    /// 房主已开始游戏（客户端据此切到牌桌）。
    GameStarting,
    /// 对局状态视图（已按本客户端裁剪隐藏信息）。每次状态变化下发。
    View { view: GameView },
    /// 对局结束，返回房间大厅。
    GameEnded,
}

/// 一张牌在客户端的渲染信息：立绘文件名 stem + 叠加的数值/标签文字。
/// 用 String（不含 game_core 的 &'static str），两端都能序列化。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct CardView {
    pub art: String,
    pub label: String,
}

/// 一个座位在对局视图里的信息。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct SeatView {
    pub name: String,
    pub chips: u32,
    pub debt: u32,
    pub committed: u32,
    pub status: PlayerStatus,
    pub is_turn: bool,
    pub is_ai: bool,
    pub in_hand: bool,
}

/// 服务器为某个客户端裁剪后的对局视图（只含它能看的信息）。
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct GameView {
    pub hand_no: u32,
    pub scene_label: String,
    pub scene_art: String,
    pub phase: Phase,
    pub pot: u32,
    pub current_bet: u32,
    pub ante: u32,
    pub aggro: f32,
    pub boss_berserk: bool,
    /// 你的座位号（None=旁观）。
    pub you_seat: Option<usize>,
    /// 轮到行动的座位（None=摊牌/结算阶段）。
    pub to_act_seat: Option<usize>,
    pub seats: Vec<SeatView>,
    pub community: Vec<CardView>,
    pub dungeon: Vec<CardView>,
    /// 各地牢节点的有效威胁（含惊动/狂暴），计分板用。
    pub dungeon_threats: Vec<u32>,
    /// 仅你自己的两张底牌。
    pub your_hole: Option<[CardView; 2]>,
    /// 摊牌时各座位亮出的底牌（按座位号；None=已弃牌或未揭示）。
    pub revealed_holes: Vec<Option<[CardView; 2]>>,
    /// 本手结算结果（摊牌后才有）。
    pub result: Option<Settlement>,
    /// 现在轮到你点选 3 张公共牌。
    pub need_select: bool,
    /// 你是房主且本手已结算，可点「下一手」。
    pub can_next: bool,
    /// 你筹码偏低，可借一锅。
    pub low_chips: bool,
}

/// 房间列表里的一条摘要（搜索房间时显示）。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct RoomInfo {
    pub code: String,
    pub name: String,
    pub players: usize,
    pub max_players: usize,
    pub in_game: bool,
}

/// 房间完整状态（房内成员看到的）。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct RoomState {
    pub code: String,
    pub name: String,
    pub max_players: usize,
    pub fill_ai: bool,
    pub host: PlayerId,
    pub players: Vec<LobbyPlayer>,
    pub in_game: bool,
}

/// 大厅里的一名玩家。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct LobbyPlayer {
    pub id: PlayerId,
    pub name: String,
}

impl ClientMsg {
    pub fn encode(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("ClientMsg 可序列化")
    }
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        serde_json::from_slice(bytes).ok()
    }
}

impl ServerMsg {
    pub fn encode(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("ServerMsg 可序列化")
    }
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        serde_json::from_slice(bytes).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_client_msg() {
        let m = ClientMsg::CreateRoom { room_name: "夜场".into(), max_players: 4, fill_ai: true };
        assert_eq!(ClientMsg::decode(&m.encode()), Some(m));
    }

    #[test]
    fn roundtrip_server_msg() {
        // ServerMsg 不派生 PartialEq（含 GameView），用编码字节对比验证往返。
        let m = ServerMsg::Joined {
            room: RoomState {
                code: "ABCD".into(),
                name: "夜场".into(),
                max_players: 4,
                fill_ai: true,
                host: 7,
                players: vec![LobbyPlayer { id: 7, name: "你".into() }],
                in_game: false,
            },
        };
        let bytes = m.encode();
        let back = ServerMsg::decode(&bytes).expect("应能解码");
        assert_eq!(bytes, back.encode());
    }
}
