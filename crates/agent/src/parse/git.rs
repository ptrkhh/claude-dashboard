use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GitStatus {
    pub branch: String,
    pub dirty: usize,
    pub ahead: u32,
    pub behind: u32,
}

/// The count following `label` in a `-b` header, e.g. "ahead 2". Only the
/// leading digits count, so "ahead 2, behind 1" reads as 2.
fn counted(hay: &str, label: &str) -> u32 {
    hay.split_once(label)
        .map(|(_, rest)| rest.trim_start().split(|c: char| !c.is_ascii_digit()).next().unwrap_or(""))
        .and_then(|d| d.parse().ok())
        .unwrap_or(0)
}

pub fn parse_git_status(out: &str) -> GitStatus {
    let lines: Vec<&str> = out.lines().filter(|l| !l.is_empty()).collect();
    let head = lines.first().copied().unwrap_or("");
    let branch = head
        .strip_prefix("## ")
        .unwrap_or(head)
        .split("...")
        .next()
        .unwrap_or("")
        .trim()
        .to_string();

    GitStatus {
        branch,
        dirty: lines.len().saturating_sub(1),
        ahead: counted(head, "ahead "),
        behind: counted(head, "behind "),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_branch_dirty_ahead_behind() {
        let out = "## main...origin/main [ahead 2, behind 1]\n M server.js\n?? new.txt\n";
        assert_eq!(
            parse_git_status(out),
            GitStatus { branch: "main".into(), dirty: 2, ahead: 2, behind: 1 }
        );
    }

    #[test]
    fn branch_with_no_upstream_has_zero_ahead_behind() {
        assert_eq!(
            parse_git_status("## feature-x\n"),
            GitStatus { branch: "feature-x".into(), dirty: 0, ahead: 0, behind: 0 }
        );
    }

    #[test]
    fn git_status_serializes_with_nodes_field_names() {
        let j = serde_json::to_string(&parse_git_status("## main...origin/main [ahead 2]\n M x\n"))
            .unwrap();
        assert_eq!(j, r#"{"branch":"main","dirty":1,"ahead":2,"behind":0}"#);
    }

    #[test]
    fn empty_output_does_not_panic() {
        assert_eq!(
            parse_git_status(""),
            GitStatus { branch: "".into(), dirty: 0, ahead: 0, behind: 0 }
        );
    }
}
