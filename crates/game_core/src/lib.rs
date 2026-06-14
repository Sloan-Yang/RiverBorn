//! RiverBorn 核心规则库（零依赖、不含引擎）。
//!
//! 这一层负责所有「能跑单元测试」的游戏逻辑：发牌、下注博弈、
//! 阶段推进、战斗结算。Bevy 表现层（riverborn crate）只读取这里的
//! 状态并驱动它，便于将来无痛接入联机 / AI。

pub mod ai;
pub mod cards;
pub mod game;
pub mod player;
pub mod rng;

pub use cards::*;
pub use game::*;
pub use player::*;
