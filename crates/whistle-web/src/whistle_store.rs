//! whistle UI 的持久化数据模型：规则分组、Values、选中状态与全局开关。
//!
//! 对应 whistle 服务端 `lib/rules` + `Storage` 的职责子集：把「默认规则 + 多个具名
//! 规则分组（可勾选）」和「具名 Values」持久化到数据目录，并据此计算**生效规则文本**
//! （默认规则 + 选中分组），供代理内核热加载。
//!
//! 持久化为单个 JSON 文件 `<data_dir>/whistle-ui.json`。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// 一个具名条目（规则分组或 Value）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListItem {
    /// 名称（在同一列表内唯一）。
    pub name: String,
    /// 文本内容（规则文本或 Value 文本）。
    #[serde(default)]
    pub data: String,
    /// 是否被选中（仅规则分组有意义；Value 恒为 false）。
    #[serde(default)]
    pub selected: bool,
}

impl ListItem {
    fn new(name: impl Into<String>, data: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            data: data.into(),
            selected: false,
        }
    }
}

/// whistle UI 的全部可编辑状态。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WhistleData {
    /// 默认规则文本（始终参与，除非被禁用）。
    pub default_rules: String,
    /// 默认规则是否被禁用。
    pub default_disabled: bool,
    /// 全局禁用所有规则。
    pub disabled_all_rules: bool,
    /// 允许多选（否则分组为单选/单选框语义）。
    pub allow_multiple_choice: bool,
    /// 反向优先（后置规则优先）。
    pub back_rules_first: bool,
    /// 具名规则分组。
    pub rules: Vec<ListItem>,
    /// 具名 Values。
    pub values: Vec<ListItem>,
    /// 规则回收站（被删除的分组）。
    pub rules_recycle: Vec<ListItem>,
    /// Values 回收站。
    pub values_recycle: Vec<ListItem>,
}

impl Default for WhistleData {
    fn default() -> Self {
        Self {
            default_rules: String::new(),
            default_disabled: false,
            disabled_all_rules: false,
            allow_multiple_choice: false,
            back_rules_first: false,
            rules: Vec::new(),
            values: Vec::new(),
            rules_recycle: Vec::new(),
            values_recycle: Vec::new(),
        }
    }
}

impl WhistleData {
    /// 从数据目录加载；文件不存在或损坏时返回 `None`。
    pub fn load(path: &Path) -> Option<Self> {
        let text = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// 持久化到数据目录（best-effort，错误仅记录日志）。
    pub fn save(&self, path: &Path) {
        match serde_json::to_string_pretty(self) {
            Ok(text) => {
                if let Some(dir) = path.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                if let Err(e) = std::fs::write(path, text) {
                    tracing::warn!(path = %path.display(), %e, "保存 whistle-ui.json 失败");
                }
            }
            Err(e) => tracing::warn!(%e, "序列化 whistle-ui.json 失败"),
        }
    }

    /// 计算当前生效的规则文本（默认规则 + 选中分组）。
    ///
    /// `back_rules_first` 为真时，选中分组排在默认规则之前（后置优先）。
    pub fn effective_text(&self) -> String {
        if self.disabled_all_rules {
            return String::new();
        }
        let mut blocks: Vec<&str> = Vec::new();
        let default_block = if self.default_disabled {
            ""
        } else {
            self.default_rules.as_str()
        };
        let selected: Vec<&str> = self
            .rules
            .iter()
            .filter(|r| r.selected)
            .map(|r| r.data.as_str())
            .collect();

        if self.back_rules_first {
            blocks.extend(selected);
            if !default_block.is_empty() {
                blocks.push(default_block);
            }
        } else {
            if !default_block.is_empty() {
                blocks.push(default_block);
            }
            blocks.extend(selected);
        }
        blocks
            .iter()
            .filter(|b| !b.trim().is_empty())
            .copied()
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// 已选中的规则分组数。
    pub fn enabled_count(&self) -> usize {
        self.rules.iter().filter(|r| r.selected).count()
    }

    // ---- 规则分组增删改 ----

    /// 查找规则分组的下标。
    fn rule_idx(&self, name: &str) -> Option<usize> {
        self.rules.iter().position(|r| r.name == name)
    }

    /// 新增或更新一个规则分组（同名则覆盖其 data）。返回是否为新建。
    pub fn add_rule(&mut self, name: &str, data: &str) -> bool {
        match self.rule_idx(name) {
            Some(i) => {
                self.rules[i].data = data.to_string();
                false
            }
            None => {
                self.rules.push(ListItem::new(name, data));
                true
            }
        }
    }

    /// 删除规则分组（移入回收站）。
    pub fn remove_rule(&mut self, name: &str) -> bool {
        match self.rule_idx(name) {
            Some(i) => {
                let item = self.rules.remove(i);
                self.rules_recycle.push(item);
                if self.rules_recycle.len() > 64 {
                    self.rules_recycle.remove(0);
                }
                true
            }
            None => false,
        }
    }

    /// 重命名规则分组。
    pub fn rename_rule(&mut self, from: &str, to: &str) -> bool {
        if self.rule_idx(to).is_some() {
            return false; // 目标名已存在
        }
        match self.rule_idx(from) {
            Some(i) => {
                self.rules[i].name = to.to_string();
                true
            }
            None => false,
        }
    }

    /// 选中规则分组。单选模式下会取消其它分组的选中。
    pub fn select_rule(&mut self, name: &str) -> bool {
        let Some(i) = self.rule_idx(name) else {
            return false;
        };
        if !self.allow_multiple_choice {
            for r in &mut self.rules {
                r.selected = false;
            }
        }
        self.rules[i].selected = true;
        true
    }

    /// 取消选中规则分组。
    pub fn unselect_rule(&mut self, name: &str) -> bool {
        match self.rule_idx(name) {
            Some(i) => {
                self.rules[i].selected = false;
                true
            }
            None => false,
        }
    }

    /// 把规则分组移动到目标分组之后（拖拽排序）。
    pub fn move_rule_to(&mut self, name: &str, to: &str) -> bool {
        move_to(&mut self.rules, name, to)
    }

    // ---- Values 增删改 ----

    fn value_idx(&self, name: &str) -> Option<usize> {
        self.values.iter().position(|v| v.name == name)
    }

    /// 新增或更新一个 Value（同名则覆盖）。返回是否为新建。
    pub fn add_value(&mut self, name: &str, data: &str) -> bool {
        match self.value_idx(name) {
            Some(i) => {
                self.values[i].data = data.to_string();
                false
            }
            None => {
                self.values.push(ListItem::new(name, data));
                true
            }
        }
    }

    /// 删除 Value（移入回收站）。
    pub fn remove_value(&mut self, name: &str) -> bool {
        match self.value_idx(name) {
            Some(i) => {
                let item = self.values.remove(i);
                self.values_recycle.push(item);
                if self.values_recycle.len() > 64 {
                    self.values_recycle.remove(0);
                }
                true
            }
            None => false,
        }
    }

    /// 重命名 Value。
    pub fn rename_value(&mut self, from: &str, to: &str) -> bool {
        if self.value_idx(to).is_some() {
            return false;
        }
        match self.value_idx(from) {
            Some(i) => {
                self.values[i].name = to.to_string();
                true
            }
            None => false,
        }
    }

    /// 移动 Value 排序。
    pub fn move_value_to(&mut self, name: &str, to: &str) -> bool {
        move_to(&mut self.values, name, to)
    }
}

/// 把 `list` 中名为 `name` 的项移动到名为 `to` 的项所在位置。
fn move_to(list: &mut Vec<ListItem>, name: &str, to: &str) -> bool {
    let Some(from_idx) = list.iter().position(|x| x.name == name) else {
        return false;
    };
    let Some(to_idx) = list.iter().position(|x| x.name == to) else {
        return false;
    };
    if from_idx == to_idx {
        return true;
    }
    let item = list.remove(from_idx);
    // remove 后目标下标可能左移一位。
    let insert_at =
        list.iter().position(|x| x.name == to).map_or(
            to_idx,
            |i| {
                if from_idx < i {
                    i + 1
                } else {
                    i
                }
            },
        );
    let insert_at = insert_at.min(list.len());
    list.insert(insert_at, item);
    true
}

/// 默认数据文件名。
pub const DATA_FILE: &str = "whistle-ui.json";

/// 数据文件完整路径。
pub fn data_path(dir: &Path) -> PathBuf {
    dir.join(DATA_FILE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_text_combines_default_and_selected() {
        let mut d = WhistleData {
            default_rules: "a.com host://1.1.1.1".into(),
            ..Default::default()
        };
        d.add_rule("g1", "b.com host://2.2.2.2");
        d.add_rule("g2", "c.com host://3.3.3.3");
        // 仅默认规则生效
        assert_eq!(d.effective_text(), "a.com host://1.1.1.1");
        // 选中 g2（单选）
        d.select_rule("g2");
        let t = d.effective_text();
        assert!(t.contains("a.com"));
        assert!(t.contains("c.com"));
        assert!(!t.contains("b.com"));
    }

    #[test]
    fn single_choice_replaces_selection() {
        let mut d = WhistleData::default();
        d.add_rule("g1", "x");
        d.add_rule("g2", "y");
        d.select_rule("g1");
        d.select_rule("g2");
        assert_eq!(d.enabled_count(), 1);
        assert!(!d.rules[0].selected);
        assert!(d.rules[1].selected);
    }

    #[test]
    fn multi_choice_keeps_multiple() {
        let mut d = WhistleData {
            allow_multiple_choice: true,
            ..Default::default()
        };
        d.add_rule("g1", "x");
        d.add_rule("g2", "y");
        d.select_rule("g1");
        d.select_rule("g2");
        assert_eq!(d.enabled_count(), 2);
    }

    #[test]
    fn disabled_all_rules_yields_empty() {
        let mut d = WhistleData {
            default_rules: "a.com host://1.1.1.1".into(),
            disabled_all_rules: true,
            ..Default::default()
        };
        assert_eq!(d.effective_text(), "");
        d.disabled_all_rules = false;
        d.default_disabled = true;
        assert_eq!(d.effective_text(), "");
    }

    #[test]
    fn rename_and_remove_and_recycle() {
        let mut d = WhistleData::default();
        d.add_rule("g1", "x");
        assert!(d.rename_rule("g1", "g2"));
        assert!(d.rule_idx("g2").is_some());
        assert!(d.remove_rule("g2"));
        assert_eq!(d.rules.len(), 0);
        assert_eq!(d.rules_recycle.len(), 1);
    }

    #[test]
    fn move_to_reorders() {
        let mut d = WhistleData::default();
        d.add_rule("a", "1");
        d.add_rule("b", "2");
        d.add_rule("c", "3");
        // 把 a 移到 c 之后
        assert!(d.move_rule_to("a", "c"));
        let names: Vec<&str> = d.rules.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["b", "c", "a"]);
    }
}
