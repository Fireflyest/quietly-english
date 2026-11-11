//! 内置词缀索引，只用于 `/root <affix>` 这类主动查询。
//!
//! **这里不负责拆词。** 单词的词根必须来自有据可查的数据（Wiktionary 词源模板、
//! 词典条目、词根词缀教材），由 `qe-store` 的 `etymology` 导入器写进 `words.roots`。
//! 早期版本用这张表做“最长匹配拆分”，会把 `surgeon` 拆出 `geo`、把 `disapproval`
//! 拆成 `dis- + -al`，属于凭空编造，已移除。
//!
//! 表内词缀释义取自通行词源学工具书对常见前缀/词根/后缀的共识释义，仅作查阅参考。

use crate::model::RootKind;

/// 一条词缀知识。
#[derive(Debug, Clone, Copy)]
pub struct Affix {
    pub part: &'static str,
    pub gloss: &'static str,
    pub kind: RootKind,
}

const fn prefix(part: &'static str, gloss: &'static str) -> Affix {
    Affix {
        part,
        gloss,
        kind: RootKind::Prefix,
    }
}

const fn root(part: &'static str, gloss: &'static str) -> Affix {
    Affix {
        part,
        gloss,
        kind: RootKind::Root,
    }
}

const fn suffix(part: &'static str, gloss: &'static str) -> Affix {
    Affix {
        part,
        gloss,
        kind: RootKind::Suffix,
    }
}

/// 常见前缀（按长度从长到短匹配更准确）。
pub const PREFIXES: &[Affix] = &[
    prefix("contra-", "相反"),
    prefix("circum-", "环绕"),
    prefix("inter-", "在……之间"),
    prefix("intra-", "在内部"),
    prefix("intro-", "向内"),
    prefix("extra-", "在……之外"),
    prefix("hyper-", "过度、超"),
    prefix("hypo-", "在下方、不足"),
    prefix("macro-", "大的"),
    prefix("micro-", "小的"),
    prefix("multi-", "多"),
    prefix("mono-", "单一"),
    prefix("omni-", "全部"),
    prefix("super-", "在上、超过"),
    prefix("trans-", "跨越"),
    prefix("under-", "在下、不足"),
    prefix("ante-", "在……之前"),
    prefix("anti-", "反对"),
    prefix("auto-", "自己"),
    prefix("bene-", "好"),
    prefix("tele-", "远"),
    prefix("post-", "在后"),
    prefix("pre-", "预先"),
    prefix("pro-", "向前"),
    prefix("retro-", "向后"),
    prefix("semi-", "一半"),
    prefix("ultra-", "极端、超越"),
    prefix("circum", "环绕"),
    prefix("con-", "共同、加强"),
    prefix("com-", "共同、加强"),
    prefix("col-", "共同"),
    prefix("cor-", "共同"),
    prefix("dis-", "分开、否定"),
    prefix("mis-", "错误"),
    prefix("non-", "不"),
    prefix("out-", "超过、向外"),
    prefix("over-", "过度"),
    prefix("sub-", "在下方"),
    prefix("suc-", "在下方"),
    prefix("suf-", "在下方"),
    prefix("sup-", "在下方"),
    prefix("sus-", "在下方"),
    prefix("syn-", "共同"),
    prefix("sym-", "共同"),
    prefix("un-", "不"),
    prefix("uni-", "单一"),
    prefix("ab-", "离开"),
    prefix("ad-", "朝向"),
    prefix("de-", "向下、去除"),
    prefix("ex-", "向外"),
    prefix("re-", "再次、向后"),
    prefix("se-", "分开"),
    prefix("in-", "向内、不"),
    prefix("im-", "向内、不"),
    prefix("il-", "不"),
    prefix("ir-", "不"),
    prefix("ob-", "朝向、反对"),
    prefix("oc-", "朝向"),
    prefix("per-", "贯穿"),
    prefix("pre", "预先"),
    prefix("pro", "向前"),
    prefix("a-", "朝向、加强"),
    prefix("e-", "向外"),
];

/// 常见词根。
pub const ROOTS: &[Affix] = &[
    root("anthrop", "人类"),
    root("chron", "时间"),
    root("cred", "相信"),
    root("cycl", "圆、循环"),
    root("dem", "人民"),
    root("dict", "说"),
    root("duct", "引导"),
    root("fac", "做"),
    root("fect", "做"),
    root("fic", "做"),
    root("fer", "携带"),
    root("fin", "结束、界限"),
    root("gen", "产生"),
    root("geo", "地球"),
    root("grad", "走、步"),
    root("gress", "走"),
    root("graph", "写、记录"),
    root("ject", "投掷"),
    root("log", "言语、学科"),
    root("manu", "手"),
    root("mit", "送"),
    root("miss", "送"),
    root("migr", "迁移"),
    root("mob", "移动"),
    root("mot", "移动"),
    root("nom", "名字、法则"),
    root("ped", "脚、儿童"),
    root("pel", "推"),
    root("puls", "推"),
    root("pend", "悬挂"),
    root("phon", "声音"),
    root("photo", "光"),
    root("port", "携带"),
    root("pos", "放置"),
    root("pon", "放置"),
    root("rupt", "破裂"),
    root("scrib", "写"),
    root("script", "写"),
    root("sens", "感觉"),
    root("sent", "感觉"),
    root("spect", "看"),
    root("spec", "看"),
    root("spir", "呼吸"),
    root("stat", "站立"),
    root("sist", "站立"),
    root("struct", "建造"),
    root("tend", "伸展"),
    root("tens", "伸展"),
    root("tempor", "时间"),
    root("terr", "土地"),
    root("therm", "热"),
    root("tract", "拉"),
    root("vac", "空"),
    root("ven", "来"),
    root("vent", "来"),
    root("vert", "转"),
    root("vers", "转"),
    root("vid", "看"),
    root("vis", "看"),
    root("viv", "生命"),
    root("vit", "生命"),
    root("voc", "喊、声音"),
    root("thesis", "放置"),
    root("tain", "握住"),
    root("labor", "劳动"),
    root("simul", "相似"),
    root("phen", "显现"),
    root("rig", "浇水"),
    root("eco", "生态"),
    root("system", "系统"),
    root("band", "束缚"),
];

/// 常见后缀。
pub const SUFFIXES: &[Affix] = &[
    suffix("ability", "能力"),
    suffix("ation", "名词后缀"),
    suffix("ition", "名词后缀"),
    suffix("ology", "学科"),
    suffix("ability", "能……的性质"),
    suffix("ible", "能够……的"),
    suffix("able", "能够……的"),
    suffix("ance", "名词后缀"),
    suffix("ence", "名词后缀"),
    suffix("ment", "名词后缀"),
    suffix("ness", "名词后缀"),
    suffix("ship", "名词后缀"),
    suffix("tion", "名词后缀"),
    suffix("sion", "名词后缀"),
    suffix("ism", "主义、学说"),
    suffix("ist", "人"),
    suffix("ity", "性质"),
    suffix("ive", "形容词后缀"),
    suffix("ous", "充满……的"),
    suffix("ful", "充满……的"),
    suffix("less", "没有……的"),
    suffix("ify", "使……"),
    suffix("ize", "使……"),
    suffix("ise", "使……"),
    suffix("ary", "形容词/名词后缀"),
    suffix("ate", "动词/形容词后缀"),
    suffix("ent", "形容词/名词后缀"),
    suffix("ant", "形容词/名词后缀"),
    suffix("al", "形容词后缀"),
    suffix("ic", "形容词后缀"),
    suffix("ly", "副词后缀"),
    suffix("ee", "被……的人"),
    suffix("er", "人、物"),
    suffix("or", "人、物"),
    suffix("ward", "朝……方向"),
    suffix("hood", "状态"),
    suffix("dom", "状态、领域"),
    suffix("y", "名词/形容词后缀"),
];

/// 全部词缀，供 `/root` 检索。
pub fn all() -> impl Iterator<Item = &'static Affix> {
    PREFIXES.iter().chain(ROOTS.iter()).chain(SUFFIXES.iter())
}

/// 精确或模糊查找一条词缀知识。
pub fn lookup(term: &str) -> Option<&'static Affix> {
    let needle = normalize(term);
    if needle.is_empty() {
        return None;
    }
    all().find(|a| normalize(a.part) == needle).or_else(|| {
        all().find(|a| needle.len() >= 3 && normalize(a.part).starts_with(needle.as_str()))
    })
}

/// 查询包含某个词根的所有内置词条。
pub fn containing(term: &str) -> Vec<&'static Affix> {
    let needle = normalize(term);
    if needle.is_empty() {
        return Vec::new();
    }
    all()
        .filter(|a| {
            let part = normalize(a.part);
            part == needle || (needle.len() >= 2 && part.contains(needle.as_str()))
        })
        .collect()
}

fn normalize(s: &str) -> String {
    s.trim()
        .trim_matches('-')
        .to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_accepts_hyphenless_input() {
        let hit = lookup("spect").expect("spect should exist");
        assert_eq!(hit.gloss, "看");
    }

    #[test]
    fn lookup_normalises_hyphens() {
        let hit = lookup("-ology").expect("ology should exist");
        assert_eq!(hit.kind, RootKind::Suffix);
        assert_eq!(hit.part, "ology");
    }

    #[test]
    fn containing_finds_related_affixes() {
        let hits = containing("olog");
        assert!(hits.iter().any(|affix| affix.part == "ology"));
    }

    #[test]
    fn empty_query_is_ignored() {
        assert!(lookup("").is_none());
        assert!(containing("").is_empty());
    }
}
