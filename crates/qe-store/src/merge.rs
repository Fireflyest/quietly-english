//! 多词典释义合并。
//!
//! ECDICT 给的是「简明英汉字典」，义项少但精；21 世纪、朗道这类词典义项更全。
//! 依次导入时不该让后一本覆盖前一本，而应把新的义项**追加**进去。
//!
//! 合并以「行」为单位，原因是渲染层把每行当成一个义项（见 `qe_core::text`）：
//!
//! ```text
//! 已有                               新词典
//! n. 目的, 缘故, 理由       +        n. 缘故, 理由, 目的, 利益, 好处
//!                                   n. 日本清酒
//! ```
//!
//! 结果保留已有顺序，只补上新的行；重复的行（忽略空白差异）会被跳过。

/// 把 `incoming` 的义项合并进 `existing`，返回合并后的文本。
///
/// 判重用的是「归一化后的整行」，不做义项级别的拆分——两个词典对同一词性的
/// 近义词列表往往不同，逐词去重会把「目的, 缘故」和「缘故, 目的」都留下，
/// 反而更乱。
pub fn merge_translations(existing: &str, incoming: &str) -> String {
    let mut lines: Vec<&str> = existing.lines().filter(|l| !l.trim().is_empty()).collect();
    let mut seen: Vec<String> = lines.iter().map(|line| normalise(line)).collect();

    for line in incoming.lines() {
        let line = line.trim_end();
        if line.trim().is_empty() {
            continue;
        }
        let key = normalise(line);
        if seen.iter().any(|existing| *existing == key) {
            continue;
        }
        seen.push(key);
        lines.push(line);
    }

    lines.join("\n")
}

/// 归一化：去掉空白、统一大小写，并把全角标点当半角，好让
/// 「n. 目的,缘故」和「n. 目的，缘故」、「[医] 库」和「【医】 库」被认成同一条。
fn normalise(line: &str) -> String {
    let collapsed: String = line
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .map(|ch| match ch {
            '，' => ',',
            '；' => ';',
            '：' => ':',
            '【' => '[',
            '】' => ']',
            other => other,
        })
        .collect::<String>()
        .to_lowercase();

    // 同一词性常有多种缩写写法，不统一就会把同一条释义存两次。
    for (long, short) in [("adv.", "ad."), ("adj.", "a."), ("prep.", "pr.")] {
        if let Some(rest) = collapsed.strip_prefix(long) {
            return format!("{short}{rest}");
        }
    }
    collapsed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_new_senses() {
        let merged = merge_translations("n. 目的, 缘故", "n. 日本清酒");
        assert_eq!(merged, "n. 目的, 缘故\nn. 日本清酒");
    }

    #[test]
    fn skips_identical_senses() {
        let merged = merge_translations("n. 目的\nvt. 检查", "vt. 检查\nn. 目的");
        assert_eq!(merged, "n. 目的\nvt. 检查", "nothing new to add");
    }

    #[test]
    fn ignores_whitespace_and_fullwidth_punctuation_differences() {
        assert_eq!(
            merge_translations("n. 目的, 缘故", "n.目的，缘故"),
            "n. 目的, 缘故",
            "same sense, different spelling"
        );
        assert_eq!(
            merge_translations("[医] 库", "【医】 库"),
            "[医] 库",
            "same label, different brackets"
        );
    }

    #[test]
    fn treats_equivalent_pos_abbreviations_as_the_same_sense() {
        assert_eq!(
            merge_translations("adv. 公平地", "ad. 公平地"),
            "adv. 公平地",
            "ad. 就是 adv."
        );
        assert_eq!(
            merge_translations("a. 公平的", "adj. 公平的"),
            "a. 公平的"
        );
    }

    #[test]
    fn keeps_existing_order_and_adds_afterwards() {
        let merged = merge_translations("n. 银行\n[医] 库", "n. 银行\nn. 河岸");
        assert_eq!(merged, "n. 银行\n[医] 库\nn. 河岸");
    }

    #[test]
    fn handles_empty_sides() {
        assert_eq!(merge_translations("", "n. 目的"), "n. 目的");
        assert_eq!(merge_translations("n. 目的", ""), "n. 目的");
        assert_eq!(merge_translations("", ""), "");
    }

    #[test]
    fn drops_blank_lines_from_either_side() {
        let merged = merge_translations("n. 目的\n\n", "\n\nn. 清酒\n\n");
        assert_eq!(merged, "n. 目的\nn. 清酒");
    }
}
