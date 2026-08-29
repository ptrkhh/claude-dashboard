pub fn project_dir_name(cwd: &str) -> String {
    cwd.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn munges_non_alphanumerics_to_dashes() {
        assert_eq!(project_dir_name("/mnt/d/git/backend"), "-mnt-d-git-backend");
    }

    #[test]
    fn dots_and_underscores_are_munged_too() {
        assert_eq!(project_dir_name("/a/b_c.d"), "-a-b-c-d");
    }
}
