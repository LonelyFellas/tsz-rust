mod cleanup;
pub mod dto;
pub mod handler;
mod lock;
mod repository;
pub mod router;
mod service;

pub use cleanup::run_worker;
pub use repository::{CacheRecord, PreviewRepository, PreviewRepositoryPort, VoiceRecord};
pub use service::{PreviewService, PreviewServiceError};

use crate::platform::storage::MAX_PRESIGN_TTL;

/// 试听缓存行的存活时长。对象与 row 同时创建、且 row 的过期时间从不续期，
/// 因此对象年龄恒等于 row 年龄，OSS 生命周期规则才能只按对象年龄安全回收。
pub const CACHE_TTL_HOURS: i64 = 24;

/// OSS 生命周期规则「当前版本 N 天后删除」的镜像，规则本体在控制台维护，
/// 见 `ops/speech-preview-lifecycle/README.md`。代码读不到规则，只能靠这个镜像值加下面的断言防止两边失配。
const LIFECYCLE_EXPIRE_DAYS: i64 = 30;

/// 留给规则加载（最长 24 小时）与每日调度的裕量。
const LIFECYCLE_SAFETY_MARGIN_DAYS: i64 = 7;

/// 对象必须在「row 过期 + 预签名 URL 失效」之后才被生命周期规则删除，
/// 否则会签出指向已删对象的 URL。放大 `CACHE_TTL_HOURS` 前必须先改控制台规则。
const _: () = assert!(
    CACHE_TTL_HOURS + MAX_PRESIGN_TTL.as_secs().div_ceil(3600) as i64
        <= (LIFECYCLE_EXPIRE_DAYS - LIFECYCLE_SAFETY_MARGIN_DAYS) * 24,
    "试听缓存 TTL 超出 OSS 生命周期规则的安全范围：先改控制台规则，再改这里"
);
