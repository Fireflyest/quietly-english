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
    pub args: &'static str,
    pub help: &'static str,
}

pub const SPECS: &[Spec] = &[
    Spec {
        name: "/study",
        args: " [n]",
        help: "start a session (n = new words)",
    },
    Spec {
        name: "/summary",
        args: "",
        help: "today's stats and the week ahead",
    },
    Spec {
        name: "/books",
        args: "",
        help: "list decks with progress",
    },
    Spec {
        name: "/book",
        args: " <slug>",
        help: "switch deck",
    },
    Spec {
        name: "/search",
        args: " <query>",
        help: "search words, meanings or roots",
    },
    Spec {
        name: "/root",
        args: " <affix>",
        help: "look up a prefix, root or suffix",
    },
    Spec {
        name: "/import",
        args: " <path>",
        help: "import csv / tsv / json / txt",
    },
    Spec {
        name: "/quiet",
        args: "",
        help: "toggle discreet mode",
    },
    Spec {
        name: "/limit",
        args: " [n]",
        help: "show or set the daily new-word limit",
    },
    Spec {
        name: "/add",
        args: " <word>",
        help: "add a word to the current deck",
    },
    Spec {
        name: "/phrase",
        args: " <word> <phrase> = <gloss>",
        help: "add a fixed expression to a word",
    },
    Spec {
        name: "/bury",
        args: "",
        help: "postpone the current card to tomorrow",
    },
    Spec {
        name: "/undo",
        args: "",
        help: "undo the last rating",
    },
    Spec {
        name: "/skip",
        args: "",
        help: "skip the current card",
    },
    Spec {
        name: "/exit",
        args: "",
        help: "end the session",
    },
    Spec {
        name: "/help",
        args: "",
        help: "show all commands",
    },
    Spec {
        name: "/quit",
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
            "bury" | "later" => Command::Bury,
            "undo" | "back" => Command::Undo,
            "skip" | "next" => Command::Skip,
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
    SPECS
        .iter()
        .filter(|spec| spec.name.starts_with(needle.as_str()))
        .collect()
}

/// Dimmed remainder of the first match, for inline ghost text.
pub fn ghost(input: &str) -> Option<String> {
    if input.trim() != input || input.contains(' ') {
        return None;
    }
    let best = matches(input).into_iter().next()?;
    let typed = input.chars().count();
    if best.name.chars().count() <= typed {
        return None;
    }
    Some(best.name.chars().skip(typed).collect())
}

/// Tab completion: fill in the command name, plus a space when unambiguous.
pub fn complete(input: &str) -> Option<String> {
    let hits = matches(input);
    let first = *hits.first()?;
    if hits.len() == 1 {
        return Some(format!("{} ", first.name));
    }
    // Longest common prefix of the candidates.
    let mut prefix = first.name.to_string();
    for spec in hits.iter().skip(1) {
        while !spec.name.starts_with(prefix.as_str()) {
            prefix.pop();
            if prefix.is_empty() {
                return None;
            }
        }
    }
    if prefix.chars().count() > input.chars().count() {
        Some(prefix)
    } else {
        None
    }
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
