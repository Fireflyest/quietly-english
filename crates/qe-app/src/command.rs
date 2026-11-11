//! Slash-command catalogue.
//!
//! Used twice: to parse input and to drive inline completion in the input box.
//! Keeping one table means the help text, the completion list and the parser can
//! never drift apart.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Help,
    Study(Option<u32>),
    Books,
    Book(String),
    Search(String),
    Root(String),
    Summary,
    Import(String),
    Quiet,
    Limit(Option<u32>),
    Add(String),
    Phrase(String),
    /// `/read <title | path>`：开始阅读。不带参数时续读上次那篇。
    Read(String),
    /// `/texts`：列出已导入的文本。
    Texts,
    /// `/mode full|intensive`：切换阅读模式。
    Mode(String),
    /// `/mark <word>`：标记/取消标记当前段里的一个词。
    Mark(String),
    /// `/words`：列出当前段的所有候选词。
    Words,
    /// `/next` `/prev`：翻页（阅读中）。
    Next,
    Prev,
    /// `/translate`：显示/隐藏译文。
    Translate,
    /// `/quiz [file]`：开始测验；给文件则先导入题目。
    Quiz(String),
    /// `/goto <n>`：跳到第 n 段。
    Goto(String),
    /// `/take <n>`：把当前段第 n 个候选词收进词书。
    Take(String),
    /// `/db`：两个数据库文件的位置与体积。
    Db,
    Bury,
    Undo,
    Skip,
    Exit,
    Quit,
    Unknown(String),
}

/// One command as advertised in the UI.
#[derive(Debug, Clone, Copy)]
pub struct Spec {
    pub name: &'static str,
    /// 短写，`|` 分隔。补全与解析都认，`/help` 里也会列出来。
    pub alias: &'static str,
    pub args: &'static str,
    pub help: &'static str,
}

impl Spec {
    /// `["/read", "/r"]`，用于补全匹配。
    pub fn forms(&self) -> impl Iterator<Item = String> + '_ {
        std::iter::once(self.name.to_string()).chain(
            self.alias
                .split('|')
                .filter(|alias| !alias.is_empty())
                .map(|alias| format!("/{alias}")),
        )
    }

    /// `"/read (/r)"`，用于帮助列表里显示短写。
    pub fn label(&self) -> String {
        if self.alias.is_empty() {
            self.name.to_string()
        } else {
            let short: Vec<String> = self
                .alias
                .split('|')
                .filter(|alias| !alias.is_empty())
                .map(|alias| format!("/{alias}"))
                .collect();
            format!("{} ({})", self.name, short.join(" "))
        }
    }
}

pub const SPECS: &[Spec] = &[
    Spec {
        name: "/study",
        alias: "s|learn",
        args: " [n]",
        help: "start a session (n = new words)",
    },
    Spec {
        name: "/summary",
        alias: "stats|report",
        args: "",
        help: "today's stats and the week ahead",
    },
    Spec {
        name: "/books",
        alias: "b",
        args: "",
        help: "list decks with progress",
    },
    Spec {
        name: "/book",
        alias: "use",
        args: " <slug>",
        help: "switch deck",
    },
    Spec {
        name: "/search",
        alias: "find|lookup",
        args: " <query>",
        help: "search words, meanings or roots",
    },
    Spec {
        name: "/root",
        alias: "roots|affix",
        args: " <affix>",
        help: "look up a prefix, root or suffix",
    },
    Spec {
        name: "/import",
        alias: "",
        args: " <path>",
        help: "import csv / tsv / json / txt",
    },
    Spec {
        name: "/quiet",
        alias: "discreet",
        args: "",
        help: "toggle discreet mode",
    },
    Spec {
        name: "/limit",
        alias: "daily",
        args: " [n]",
        help: "show or set the daily new-word limit",
    },
    Spec {
        name: "/add",
        alias: "",
        args: " <word>",
        help: "add a word to the current deck",
    },
    Spec {
        name: "/phrase",
        alias: "collocation",
        args: " <word> <phrase> = <gloss>",
        help: "add a fixed expression to a word",
    },
    Spec {
        name: "/read",
        alias: "r",
        args: " [title|path]",
        help: "read a text, or resume the last one",
    },
    Spec {
        name: "/texts",
        alias: "library",
        args: "",
        help: "list imported texts",
    },
    Spec {
        name: "/mode",
        alias: "",
        args: " [full|intensive]",
        help: "switch between full reading and close reading",
    },
    Spec {
        name: "/mark",
        alias: "m",
        args: " [word|all]",
        help: "mark a word, or every word in the paragraph",
    },
    Spec {
        name: "/words",
        alias: "",
        args: "",
        help: "list the words in the current paragraph",
    },
    Spec {
        name: "/next",
        alias: "n",
        args: "",
        help: "next paragraph",
    },
    Spec {
        name: "/prev",
        alias: "p",
        args: "",
        help: "previous paragraph",
    },
    Spec {
        name: "/goto",
        alias: "g",
        args: " <n>",
        help: "jump to paragraph n",
    },
    Spec {
        name: "/take",
        alias: "keep",
        args: " <n>",
        help: "add the nth word of this paragraph to the deck",
    },
    Spec {
        name: "/translate",
        alias: "tr",
        args: "",
        help: "show or hide the translation",
    },
    Spec {
        name: "/quiz",
        alias: "test",
        args: " [file]",
        help: "answer the questions for this text, or import a quiz file",
    },
    Spec {
        name: "/db",
        alias: "files|where",
        args: "",
        help: "where the study and dictionary databases live",
    },
    Spec {
        name: "/bury",
        alias: "later",
        args: "",
        help: "postpone the current card to tomorrow",
    },
    Spec {
        name: "/undo",
        alias: "back",
        args: "",
        help: "undo the last rating",
    },
    Spec {
        name: "/skip",
        alias: "",
        args: "",
        help: "skip the current card",
    },
    Spec {
        name: "/exit",
        alias: "stop|end",
        args: "",
        help: "end the session",
    },
    Spec {
        name: "/help",
        alias: "h|?",
        args: "",
        help: "show all commands",
    },
    Spec {
        name: "/quit",
        alias: "q",
        args: "",
        help: "quit quietly",
    },
];

impl Command {
    /// `None` when the text is not a command (plain lookup).
    pub fn parse(raw: &str) -> Option<Command> {
        let body = raw.trim().strip_prefix('/')?;
        let body = body.trim();
        let mut parts = body.splitn(2, char::is_whitespace);
        let name = parts.next().unwrap_or("").to_ascii_lowercase();
        let arg = parts.next().unwrap_or("").trim().to_string();

        Some(match name.as_str() {
            "" => Command::Unknown(String::new()),
            "help" | "h" | "?" => Command::Help,
            "study" | "s" | "learn" => Command::Study(arg.parse::<u32>().ok()),
            "books" | "b" => Command::Books,
            "book" | "use" => Command::Book(arg),
            "search" | "find" | "lookup" => Command::Search(arg),
            "root" | "roots" | "affix" => Command::Root(arg),
            "summary" | "stats" | "report" => Command::Summary,
            "import" => Command::Import(arg),
            "quiet" | "discreet" => Command::Quiet,
            "limit" | "daily" => Command::Limit(arg.parse::<u32>().ok()),
            "add" => Command::Add(arg),
            "phrase" | "collocation" => Command::Phrase(arg),
            "read" | "r" => Command::Read(arg),
            "texts" | "library" => Command::Texts,
            "mode" => Command::Mode(arg),
            "mark" | "m" => Command::Mark(arg),
            "words" => Command::Words,
            "next" | "n" => Command::Next,
            "prev" | "p" => Command::Prev,
            "translate" | "tr" => Command::Translate,
            "quiz" | "test" => Command::Quiz(arg),
            "goto" | "g" => Command::Goto(arg),
            "take" | "keep" => Command::Take(arg),
            "db" | "files" | "where" => Command::Db,
            "bury" | "later" => Command::Bury,
            "undo" | "back" => Command::Undo,
            "skip" => Command::Skip,
            "exit" | "stop" | "end" => Command::Exit,
            "quit" | "q" => Command::Quit,
            other => Command::Unknown(other.to_string()),
        })
    }
}

/// Commands whose name starts with `input` (`input` must start with `/`).
pub fn matches(input: &str) -> Vec<&'static Spec> {
    let needle = input.trim().to_ascii_lowercase();
    if !needle.starts_with('/') || needle.contains(' ') {
        return Vec::new();
    }
    // 别名也算命中：输入 `/n` 要能提示 `/next`，而不是一片空白。
    // 规范名前缀命中排在前面，这样 `/st` 的首选项是 `/study` 而不是 `/summary`
    // （后者的别名是 `stats`），ghost 补全才不会补到意外的方向。
    let mut hits: Vec<&'static Spec> = SPECS
        .iter()
        .filter(|spec| spec.forms().any(|form| form.starts_with(needle.as_str())))
        .collect();
    hits.sort_by_key(|spec| !spec.name.starts_with(needle.as_str()));
    hits
}

/// Dimmed remainder of the first match, for inline ghost text.
pub fn ghost(input: &str) -> Option<String> {
    if input.trim() != input || input.contains(' ') {
        return None;
    }
    let typed = input.to_ascii_lowercase();
    let spec = matches(input).into_iter().next()?;
    // 用实际命中的那个写法补全：输入 `/n` 补成 `/next`，不是 `/next` 硬套。
    let form = spec
        .forms()
        .find(|form| form.starts_with(typed.as_str()))?;
    if form.chars().count() <= typed.chars().count() {
        return None;
    }
    Some(form.chars().skip(typed.chars().count()).collect())
}

/// Tab completion: fill in the command name, plus a space when unambiguous.
pub fn complete(input: &str) -> Option<String> {
    let typed = input.to_ascii_lowercase();
    let hits = matches(input);
    if hits.is_empty() {
        return None;
    }
    // 先只看规范名前缀命中。否则 `/st` 会被 `/summary` 的别名 `stats` 拖成
    // 两个候选，公共前缀又恰好等于输入，tab 就什么也不做。
    let named: Vec<&'static Spec> = hits
        .iter()
        .copied()
        .filter(|spec| spec.name.starts_with(typed.as_str()))
        .collect();
    let pool: Vec<&'static Spec> = if named.is_empty() { hits } else { named };
    if pool.len() == 1 {
        // 唯一命中时补成规范名（`/n` → `/next`），别名只是入口不是终点。
        return Some(format!("{} ", pool[0].name));
    }
    // 多个命中时取公共前缀。
    let mut prefix = pool[0].name.to_string();
    for spec in pool.iter().skip(1) {
        while !spec.name.starts_with(prefix.as_str()) {
            prefix.pop();
            if prefix.is_empty() {
                return None;
            }
        }
    }
    (prefix.chars().count() > typed.chars().count()).then_some(prefix)
}

/// True when the input is `/<cmd> <partial>` and the command takes an argument.
pub fn arg_completion_target(input: &str) -> Option<(&'static str, &str)> {
    let body = input.strip_prefix('/')?;
    if !body.contains(' ') {
        return None;
    }
    let mut parts = body.splitn(2, ' ');
    let name = format!("/{}", parts.next()?);
    let partial = parts.next().unwrap_or("");
    let spec = SPECS
        .iter()
        .find(|spec| spec.name.eq_ignore_ascii_case(&name))?;
    if spec.args.is_empty() {
        return None;
    }
    Some((spec.name, partial))
}

/// Complete the argument of a command against a candidate list.
///
/// Matches case-insensitively on the prefix and returns the unique completion, or
/// the longest common prefix when several candidates match.
pub fn complete_arg(candidates: &[String], partial: &str) -> Option<String> {
    let needle = partial.to_ascii_lowercase();
    let hits: Vec<&String> = candidates
        .iter()
        .filter(|candidate| candidate.to_ascii_lowercase().starts_with(needle.as_str()))
        .collect();
    let first = *hits.first()?;
    if hits.len() == 1 {
        return Some(first.clone());
    }
    let mut prefix = first.clone();
    for candidate in hits.iter().skip(1) {
        while !candidate
            .to_ascii_lowercase()
            .starts_with(prefix.to_ascii_lowercase().as_str())
        {
            prefix.pop();
            if prefix.is_empty() {
                return None;
            }
        }
    }
    (prefix.chars().count() > partial.chars().count()).then_some(prefix)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_study_with_limit() {
        assert_eq!(Command::parse("/study 30"), Some(Command::Study(Some(30))));
        assert_eq!(Command::parse("/study"), Some(Command::Study(None)));
        assert_eq!(Command::parse("/s 12"), Some(Command::Study(Some(12))));
    }

    #[test]
    fn parses_search_with_spaces() {
        assert_eq!(
            Command::parse("/search  look   up "),
            Some(Command::Search("look   up".into()))
        );
    }

    #[test]
    fn plain_text_is_not_a_command() {
        assert_eq!(Command::parse("inspect"), None);
        assert_eq!(Command::parse(""), None);
    }

    #[test]
    fn unknown_command_keeps_name() {
        assert_eq!(
            Command::parse("/frobnicate 1"),
            Some(Command::Unknown("frobnicate".into()))
        );
    }

    #[test]
    fn completion_matches_prefixes() {
        assert!(matches("/st").iter().any(|s| s.name == "/study"));
        assert!(matches("/").len() >= 8);
        assert!(matches("/study 1").is_empty());
        assert_eq!(ghost("/st"), Some("udy".to_string()));
        assert_eq!(complete("/st"), Some("/study ".to_string()));
    }

    #[test]
    fn completion_uses_common_prefix_when_ambiguous() {
        // `/boo` matches /books and /book → the shared prefix stays `/book`.
        assert_eq!(complete("/boo"), Some("/book".to_string()));
        // `/b` also matches /bury, so there is no longer a useful prefix.
        assert_eq!(complete("/b"), None);
    }

    #[test]
    fn arg_completion_targets_commands_with_args() {
        assert_eq!(arg_completion_target("/book ce"), Some(("/book", "ce")));
        assert_eq!(arg_completion_target("/import ./a"), Some(("/import", "./a")));
        // No argument typed yet.
        assert_eq!(arg_completion_target("/book"), None);
        // Commands without arguments are ignored.
        assert_eq!(arg_completion_target("/books "), None);
    }

    #[test]
    fn arg_completion_fills_unique_and_shared_prefixes() {
        let decks = vec!["cet4".to_string(), "cet6".to_string(), "toefl".to_string()];
        assert_eq!(complete_arg(&decks, "cet4"), Some("cet4".to_string()));
        // `cet` matches cet4 and cet6 → nothing longer than the input.
        assert_eq!(complete_arg(&decks, "cet"), None);
        assert_eq!(complete_arg(&decks, "toe"), Some("toefl".to_string()));
        assert_eq!(complete_arg(&decks, "zz"), None);
    }

    #[test]
    fn parses_undo_bury_limit_and_add() {
        assert_eq!(Command::parse("/undo"), Some(Command::Undo));
        assert_eq!(Command::parse("/bury"), Some(Command::Bury));
        assert_eq!(Command::parse("/limit 30"), Some(Command::Limit(Some(30))));
        assert_eq!(Command::parse("/limit"), Some(Command::Limit(None)));
        assert_eq!(Command::parse("/add serendipity"), Some(Command::Add("serendipity".into())));
    }
}
