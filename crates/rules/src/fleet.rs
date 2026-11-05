//! 臨時機器（Blacksmith 這類按需開機、跑完就下線的 runner）與自家機器的分類。純規則，只看名稱形狀與 job label。
//!
//! 亂數尾巴：名稱以 `-` 切段，最後一段長度 ≥ 10、只含 `[a-z0-9]`，而且下列任一成立：
//! - (a) 同時含數字與字母；
//! - (b) 去掉最後一段後的組鍵，和名冊上另一台符合 (a) 的 runner 相同；
//! - (c) 組鍵等於這一輪任何一個 job 的某個 label（不分大小寫）。
//!
//! (b)(c) 是必要的：16 碼 `[a-z0-9]` 亂數完全沒有數字的機率約 0.5%（(26/36)^16），一天開幾十台就會遇到；
//! 只靠 (a) 的話，那一台會被當成自家機器並以亂數名稱出現在畫面上。
//! 設計見 `docs/superpowers/specs/2026-11-05-runner-sources-layout-design.md`〈分類規則〉。
//!
//! 中立：這裡不認任何廠商名稱，也不讀任何 org 設定，只看 GitHub API 本來就給的名稱與 label。

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// 亂數尾巴至少這麼長。
const MIN_TAIL_LEN: usize = 10;

/// 臨時機器所屬的組：畫面上以組為單位顯示，不顯示亂數名稱。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct PoolTag {
    /// 組鍵（小寫，去掉亂數尾巴）。
    pub stem: String,
    /// 組名：組鍵的第一段。
    pub name: String,
    /// 機型：組鍵的其餘部分，原樣；沒有就是空字串。
    pub machine: String,
}

/// 由組鍵得到顯示用的 `PoolTag`。
pub fn pool_tag(stem: &str) -> PoolTag {
    let (name, machine) = stem.split_once('-').unwrap_or((stem, ""));
    PoolTag {
        stem: stem.to_string(),
        name: name.to_string(),
        machine: machine.to_string(),
    }
}

/// 一個 scope 在這一輪看到的名冊與 job label，用來判斷哪些 runner 是臨時機器。
#[derive(Debug, Clone, Default)]
pub struct Fleet {
    /// 名冊上尾巴形狀符合的 runner 的組鍵（不論 a／b／c 哪一條）。
    shaped_stems: BTreeSet<String>,
    /// 其中符合規則 (a) 的組鍵。
    mixed_stems: BTreeSet<String>,
    /// 這一輪所有 job label（小寫）。
    labels: BTreeSet<String>,
}

/// 名稱的「組鍵（小寫）＋尾巴」，尾巴形狀不符時是 `None`。
fn split_tail(name: &str) -> Option<(String, &str)> {
    let (head, tail) = name.rsplit_once('-')?;
    let shaped = !head.is_empty()
        && tail.len() >= MIN_TAIL_LEN
        && tail
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
    shaped.then(|| (head.to_ascii_lowercase(), tail))
}

/// 規則 (a)：尾巴同時含數字與字母。
fn mixed(tail: &str) -> bool {
    tail.bytes().any(|b| b.is_ascii_digit()) && tail.bytes().any(|b| b.is_ascii_lowercase())
}

impl Fleet {
    pub fn observe<'a>(
        runner_names: impl IntoIterator<Item = &'a str>,
        job_labels: impl IntoIterator<Item = &'a str>,
    ) -> Fleet {
        let mut fleet = Fleet::default();
        for (stem, tail) in runner_names.into_iter().filter_map(split_tail) {
            if mixed(tail) {
                fleet.mixed_stems.insert(stem.clone());
            }
            fleet.shaped_stems.insert(stem);
        }
        fleet.labels = job_labels
            .into_iter()
            .map(str::to_ascii_lowercase)
            .collect();
        fleet
    }

    /// 臨時機器回傳小寫組鍵；自家機器回傳 `None`。
    pub fn stem_of(&self, runner_name: &str) -> Option<String> {
        let (stem, tail) = split_tail(runner_name)?;
        let pool = mixed(tail) || self.mixed_stems.contains(&stem) || self.labels.contains(&stem);
        pool.then_some(stem)
    }

    /// 任一 label（小寫後）等於名冊上某個臨時機器的組鍵時回傳該組鍵：排隊中的 job 在等哪一組。
    /// 名冊上尾巴形狀符合、而且組鍵等於一個 job label 的 runner，依規則 (c) 本來就是臨時機器，
    /// 所以只要 label 等於名冊上任何一個形狀符合的組鍵就成立。
    pub fn waits_for(&self, job_labels: &[String]) -> Option<String> {
        job_labels
            .iter()
            .map(|l| l.to_ascii_lowercase())
            .find(|l| self.shaped_stems.contains(l))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    type Scenario<'a> = (Vec<(&'a str, &'a str, &'a str)>, Vec<&'a str>);

    #[test]
    fn fixture_cases_match() {
        let table = include_str!("../../../tests/fixtures/runner-names.tsv");
        // scenario -> (runner 列, label 列)
        let mut scenarios: BTreeMap<&str, Scenario> = BTreeMap::new();
        for line in table
            .lines()
            .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        {
            let cols: Vec<&str> = line.split('\t').collect();
            assert_eq!(cols.len(), 5, "案例表每列要有 5 欄：{line}");
            let entry = scenarios.entry(cols[0]).or_default();
            match cols[1] {
                "runner" => entry.0.push((cols[2], cols[3], cols[4])),
                "label" => entry.1.push(cols[2]),
                other => panic!("kind 只能是 runner 或 label：{other}"),
            }
        }
        assert!(scenarios.len() >= 10, "案例表不該被清空");
        for (name, (runners, labels)) in scenarios {
            let fleet = Fleet::observe(runners.iter().map(|r| r.0), labels.iter().copied());
            for (value, expect, stem) in runners {
                let got = fleet.stem_of(value);
                match expect {
                    "own" => assert_eq!(got, None, "[{name}] {value} 應是自家機器"),
                    "pool" => {
                        assert_eq!(got.as_deref(), Some(stem), "[{name}] {value} 應是臨時機器")
                    }
                    other => panic!("expect 只能是 pool 或 own：{other}"),
                }
            }
        }
    }

    #[test]
    fn pool_tag_splits_name_and_machine() {
        assert_eq!(
            pool_tag("blacksmith-8vcpu-ubuntu-2404"),
            PoolTag {
                stem: "blacksmith-8vcpu-ubuntu-2404".into(),
                name: "blacksmith".into(),
                machine: "8vcpu-ubuntu-2404".into()
            }
        );
        assert_eq!(
            pool_tag("pool"),
            PoolTag {
                stem: "pool".into(),
                name: "pool".into(),
                machine: String::new()
            }
        );
    }

    #[test]
    fn waits_for_matches_a_known_pool_label_case_insensitively() {
        let fleet = Fleet::observe(["blacksmith-8vcpu-ubuntu-2404-4xrpwldxcceymzb7"], []);
        assert_eq!(fleet.waits_for(&["self-hosted".into()]), None);
        assert_eq!(
            fleet.waits_for(&["Blacksmith-8vcpu-Ubuntu-2404".into()]),
            Some("blacksmith-8vcpu-ubuntu-2404".into())
        );
        // 名冊上沒有這一組就認不出來（設計已接受）
        let empty = Fleet::observe([], []);
        assert_eq!(
            empty.waits_for(&["Blacksmith-8vcpu-Ubuntu-2404".into()]),
            None
        );
    }
}
