/// tmux `-F` format. The path is LAST so that a `|` inside a directory name
/// cannot shift later fields: `splitn(4, '|')` leaves it as the remainder.
/// `\x1f` is not usable as a delimiter — tmux emits the four printable bytes
/// `\037` rather than the control character.
pub const PANE_FORMAT: &str =
    "#{session_name}|#{pane_pid}|#{session_created}|#{pane_current_path}";

#[derive(Debug, Clone, PartialEq)]
pub struct Pane {
    pub name: String,
    pub pid: i32,
    pub path: String,
    pub created: i64,
}

pub fn parse_tmux_panes(out: &str) -> Vec<Pane> {
    out.lines()
        .filter(|l| !l.is_empty())
        .filter_map(|l| {
            let mut it = l.splitn(4, '|');
            let name = it.next()?;
            let pid = it.next()?.parse::<i32>().ok()?;
            let created = it.next()?.parse::<i64>().ok()?;
            let path = it.next()?;
            Some(Pane {
                name: name.to_string(),
                pid,
                path: path.to_string(),
                created,
            })
        })
        .filter(|p| p.name.starts_with("cdash-"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_to_cdash_prefixed_sessions() {
        let out = "cdash-backend-1531|4242|1785050000|/mnt/d/git/backend\n\
                   other|1|1785050001|/tmp\n";
        assert_eq!(
            parse_tmux_panes(out),
            vec![Pane {
                name: "cdash-backend-1531".into(),
                pid: 4242,
                path: "/mnt/d/git/backend".into(),
                created: 1785050000,
            }]
        );
    }

    #[test]
    fn a_pipe_in_the_path_does_not_shift_fields() {
        // The defect this port closes: with the path third of four, this line
        // put "/mnt/d/we" in `path` and "ird" where `created` belonged.
        let out = "cdash-x-0900|7|1785050000|/mnt/d/we|ird|dir\n";
        let panes = parse_tmux_panes(out);
        assert_eq!(panes.len(), 1);
        assert_eq!(panes[0].path, "/mnt/d/we|ird|dir");
        assert_eq!(panes[0].created, 1785050000);
        assert_eq!(panes[0].pid, 7);
    }

    #[test]
    fn format_string_puts_path_last() {
        assert!(PANE_FORMAT.ends_with("#{pane_current_path}"));
    }

    #[test]
    fn malformed_lines_are_skipped_not_panicked_on() {
        assert_eq!(parse_tmux_panes("cdash-broken|notanum\n\n"), vec![]);
    }
}
