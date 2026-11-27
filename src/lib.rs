//! ai2048 —— 2048 AI 服务器核心库（零第三方依赖）
//!
//! 模块划分：
//! - `board`  : 位棋盘、行/列移动表、随机数
//! - `eval`   : 评估（内置 nneonneo 式启发式表 / TD 学习权重表）
//! - `engine` : expectimax 搜索、置换表池、迭代加深、全自动思考预算、Tile-DG
//! - `learn`  : OTD/OTC/OTD+TC 学习器（TDL2048+ 同族方法，多阶段）
//! - `json`   : 极简 JSON 解析/转义
//! - `ws`     : RFC6455 WebSocket（SHA1 + base64 + 帧编解码）
//! - `http`   : 极简 HTTP 服务器与路由
//! - `color`  : CLI 终端配色（Catppuccin Mocha/Latte，含降级链）
//! - `ui`     : 内嵌前端页面

pub mod board;
pub mod color;
pub mod engine;
pub mod eval;
pub mod http;
pub mod json;
pub mod learn;
pub mod ui;
pub mod ws;
