//! 「建議清除」的判斷與理由：清理頁的「建議」標籤和儀表板的建議清單共用。

use std::collections::HashMap;
use std::path::Path;

use crate::app::{App, ModelRow, RuleRow};
use crate::models::ModelDel;
use crate::projects::Artifact;
use crate::rules::{Action, Risk};
use crate::{fmt, win};

/// 規則：安全、有東西可清、權限足夠、不是外部指令
pub fn rule(row: &RuleRow, elevated: bool) -> bool {
    row.bytes() > 0
        && row.rule.risk == Risk::Safe
        && (!row.rule.admin || elevated)
        && !matches!(row.rule.action, Action::Command { .. })
}

/// 專案產物：安全，而且 7 天以上沒有變動
pub fn artifact(a: &Artifact) -> bool {
    a.kind.risk == Risk::Safe && a.newest < win::now_filetime().saturating_sub(7 * win::FILETIME_DAY)
}

pub fn artifact_why(a: &Artifact) -> String {
    format!("{}，裡面最新的檔案是{}修改的。{}", a.kind.label, fmt::ago(a.newest), a.kind.hint)
}

/// 每個模型若是「重複且不是保留的那份」，回傳保留那份的索引。
pub fn model_dups(models: &[ModelRow]) -> Vec<Option<usize>> {
    let mut groups: HashMap<(String, u64), Vec<usize>> = HashMap::new();
    for (i, m) in models.iter().enumerate() {
        if m.m.del == ModelDel::File {
            groups.entry((m.m.name.to_lowercase(), m.m.bytes)).or_default().push(i);
        }
    }
    let mut out = vec![None; models.len()];
    for idx in groups.values().filter(|v| v.len() > 1) {
        let keep = *idx.iter().max_by_key(|&&i| models[i].m.newest).unwrap();
        for &i in idx {
            if i != keep {
                out[i] = Some(keep);
            }
        }
    }
    out
}

pub fn model_why(kept: &Path) -> String {
    format!("同名、同大小的檔案在 {} 也有一份。保留最新的那份就夠了。", kept.display())
}

pub fn drive_of(p: &Path) -> Option<char> {
    let s = p.to_string_lossy();
    let b = s.as_bytes();
    (b.len() >= 2 && b[1] == b':').then(|| (b[0] as char).to_ascii_uppercase())
}

pub enum Ref {
    Rule(usize),
    Art(usize),
    Model(usize),
}

pub struct Sugg {
    pub title: String,
    pub why: String,
    pub bytes: u64,
    /// 所在磁碟（跨多顆時為 None）
    pub drive: Option<char>,
    pub r: Ref,
}

/// 依大小排序的建議清單；drive=Some 時只算那顆磁碟上的部分。
pub fn list(app: &App, drive: Option<char>) -> Vec<Sugg> {
    const MIN: u64 = 50 << 20;
    let on = |p: &Path| drive.is_none() || drive_of(p) == drive;
    let mut out = Vec::new();
    for (i, row) in app.clean.rows.iter().enumerate() {
        if !rule(row, app.elevated) {
            continue;
        }
        let Some(m) = &row.measured else { continue };
        let found: Vec<_> = m.found.iter().filter(|f| on(&f.path)).collect();
        let bytes: u64 = found.iter().map(|f| f.bytes).sum();
        if bytes < MIN {
            continue;
        }
        let mut drives: Vec<char> = found.iter().filter_map(|f| drive_of(&f.path)).collect();
        drives.sort();
        drives.dedup();
        out.push(Sugg {
            title: row.rule.name.clone(),
            why: row.rule.why(),
            bytes,
            drive: (drives.len() == 1).then(|| drives[0]),
            r: Ref::Rule(i),
        });
    }
    for (i, a) in app.clean.proj.items.iter().enumerate() {
        if !artifact(&a.art) || a.art.bytes < MIN || !on(&a.art.path) {
            continue;
        }
        let project = a.art.path.parent().map(|p| p.display().to_string()).unwrap_or_default();
        out.push(Sugg {
            title: format!("{}　{}", a.art.kind.label, project),
            why: artifact_why(&a.art),
            bytes: a.art.bytes,
            drive: drive_of(&a.art.path),
            r: Ref::Art(i),
        });
    }
    let dups = model_dups(&app.clean.proj.models);
    for (i, m) in app.clean.proj.models.iter().enumerate() {
        let Some(keep) = dups[i] else { continue };
        if !on(&m.m.path) {
            continue;
        }
        out.push(Sugg {
            title: format!("重複的模型　{}", m.m.name),
            why: model_why(&app.clean.proj.models[keep].m.path),
            bytes: m.m.bytes,
            drive: drive_of(&m.m.path),
            r: Ref::Model(i),
        });
    }
    out.sort_by(|a, b| b.bytes.cmp(&a.bytes));
    out
}

pub fn is_checked(app: &App, r: &Ref) -> bool {
    match *r {
        Ref::Rule(i) => app.clean.rows[i].checked,
        Ref::Art(i) => app.clean.proj.items[i].checked,
        Ref::Model(i) => app.clean.proj.models[i].checked,
    }
}

pub fn set_checked(app: &mut App, r: &Ref, v: bool) {
    match *r {
        Ref::Rule(i) => app.clean.rows[i].checked = v,
        Ref::Art(i) => app.clean.proj.items[i].checked = v,
        Ref::Model(i) => app.clean.proj.models[i].checked = v,
    }
}
