//! 撤销快照的持久化。
//!
//! **格式与 Python 版完全一致**, 两侧可以互相读取同一个 `.icon_arranger_state.json`:
//!
//! ```json
//! {
//!   "saved_at": "2026-11-25 17:00:00",
//!   "positions": [["名称", x, y], ...],
//!   "history": [ { "saved_at": "...", "positions": [...] }, ... ]
//! }
//! ```

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// 一层快照: 一批图标的位置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Layer {
    /// 快照保存时间
    #[serde(default)]
    pub saved_at: Option<String>,
    /// 图标位置: [名称, x, y]
    ///
    /// 用 `serde_json::Value` 是因为名称是字符串而坐标是数字, 且旧版还可能
    /// 出现 4 元组 [index, name, x, y] 格式。
    #[serde(default)]
    pub positions: Vec<Vec<serde_json::Value>>,
}

/// 完整的状态文件
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StateFile {
    /// 当前层快照时间
    #[serde(default)]
    pub saved_at: Option<String>,
    /// 当前层图标位置
    #[serde(default)]
    pub positions: Vec<Vec<serde_json::Value>>,
    /// 历史层 (最多 `HISTORY_KEEP` 层)
    #[serde(default)]
    pub history: Vec<Layer>,
}

impl StateFile {
    /// 按深度挑出一层快照。
    ///
    /// depth=0 是当前层, depth=1 是上一层, 依此类推。
    pub fn pick(&self, depth: usize) -> anyhow::Result<(Vec<(String, i32, i32)>, String)> {
        let current = Layer {
            saved_at: self.saved_at.clone(),
            positions: self.positions.clone(),
        };
        let mut layers: Vec<&Layer> = vec![&current];
        for h in &self.history {
            layers.push(h);
        }
        let valid: Vec<&Layer> = layers.into_iter().filter(|l| !l.positions.is_empty()).collect();
        if valid.is_empty() {
            anyhow::bail!("位置记录为空");
        }
        if depth >= valid.len() {
            anyhow::bail!("--depth 超出范围: 可回退 0..{} (共 {} 层)", valid.len() - 1, valid.len());
        }
        let layer = valid[depth];
        let mut records = Vec::with_capacity(layer.positions.len());
        for rec in &layer.positions {
            // 兼容旧版 4 元组格式 [index, name, x, y]
            let (name_idx, x_idx, y_idx) = if rec.len() == 4 {
                (1usize, 2usize, 3usize)
            } else {
                (0usize, 1usize, 2usize)
            };
            let name = rec.get(name_idx).and_then(|v| v.as_str()).unwrap_or("").to_string();
            let x = rec.get(x_idx).and_then(|v| v.as_i64()).unwrap_or(0) as i32;
            let y = rec.get(y_idx).and_then(|v| v.as_i64()).unwrap_or(0) as i32;
            records.push((name, x, y));
        }
        Ok((records, layer.saved_at.clone().unwrap_or_else(|| "未知时间".to_string())))
    }
}

/// 状态文件存储。
#[derive(Clone)]
pub struct StateStore {
    path: PathBuf,
}

impl StateStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 读取状态文件 (不存在或损坏时返回 `None`)。
    pub fn load(&self) -> Option<StateFile> {
        let text = std::fs::read_to_string(&self.path).ok()?;
        serde_json::from_str::<StateFile>(&text).ok()
    }

    /// 保存一批图标位置, 并把旧的一层压入历史。
    ///
    /// 返回压入后的历史层数。
    pub fn save(&self, icons: &[(String, i32, i32)], now: &str) -> anyhow::Result<usize> {
        let mut history: Vec<Layer> = Vec::new();

        if let Some(old) = self.load() {
            if !old.positions.is_empty() {
                history.push(Layer {
                    saved_at: old.saved_at,
                    positions: old.positions,
                });
            }
            for h in old.history {
                if !h.positions.is_empty() {
                    history.push(h);
                }
            }
        }

        let data = StateFile {
            saved_at: Some(now.to_string()),
            positions: icons
                .iter()
                .map(|(n, x, y)| {
                    vec![
                        serde_json::Value::String(n.clone()),
                        serde_json::Value::from(*x as i64),
                        serde_json::Value::from(*y as i64),
                    ]
                })
                .collect(),
            history: history.into_iter().take(crate::HISTORY_KEEP).collect(),
        };

        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(&data)?;
        std::fs::write(&self.path, text)?;
        Ok(data.history.len())
    }

    /// 列出所有历史层 (供前端展示快照列表)。
    pub fn list(&self) -> Vec<(usize, Option<String>, usize)> {
        let Some(data) = self.load() else {
            return vec![];
        };
        let mut layers: Vec<(usize, Option<String>, usize)> = Vec::new();
        if !data.positions.is_empty() {
            layers.push((0, data.saved_at.clone(), data.positions.len()));
        }
        for (i, h) in data.history.iter().enumerate() {
            if !h.positions.is_empty() {
                layers.push((i + 1, h.saved_at.clone(), h.positions.len()));
            }
        }
        layers
    }

    /// 清空历史 (只保留当前层)。
    pub fn clear_history(&self) -> anyhow::Result<()> {
        let Some(mut data) = self.load() else {
            return Ok(());
        };
        data.history.clear();
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&self.path, serde_json::to_string_pretty(&data)?)?;
        Ok(())
    }
}

/// 生成当前时间戳字符串 (`YYYY-MM-DD HH:MM:SS`), 与 Python 版格式一致。
pub fn now_string() -> String {
    use windows::Win32::Foundation::SYSTEMTIME;
    use windows::Win32::System::SystemInformation::GetLocalTime;
    let st: SYSTEMTIME = unsafe { GetLocalTime() };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute, st.wSecond
    )
}
