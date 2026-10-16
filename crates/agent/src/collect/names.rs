use std::path::Path;

const ANIMALS: [&str; 32] = [
    "cat", "dog", "rabbit", "mouse", "horse", "donkey", "cow", "pig", "sheep", "goat", "chicken",
    "duck", "goose", "lion", "tiger", "bear", "panda", "elephant", "giraffe", "zebra", "monkey",
    "hippo", "kangaroo", "koala", "fox", "wolf", "deer", "penguin", "parrot", "dolphin", "whale",
    "turtle",
];

/// A random UUID also lets Claude know its stable session identity before launch.
pub fn new_sid() -> Result<String, super::validate::Refused> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes)
        .map_err(|e| super::validate::Refused::Failed(format!("session randomness: {e}")))?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    ))
}

pub fn session_name(agent: &str, dir: &str, sid: &str) -> String {
    let project = Path::new(dir)
        .file_name()
        .unwrap_or_else(|| Path::new(dir).as_os_str());
    // UUID tail is random and stable across resume; duplicate animals are welcome.
    let tail = sid.rsplit('-').next().unwrap_or_default();
    let animal = ANIMALS
        [(u64::from_str_radix(tail, 16).unwrap_or_default() % ANIMALS.len() as u64) as usize];
    format!("[{agent}] {} · {animal}", project.to_string_lossy())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn familiar_animals_repeat_without_suffixes_and_survive_resume() {
        for (i, animal) in ANIMALS.iter().enumerate() {
            let sid = format!("01234567-1234-4234-8234-{i:012x}");
            assert_eq!(
                session_name("Codex", "/git/claude-dashboard", &sid),
                format!("[Codex] claude-dashboard · {animal}")
            );
            let duplicate = format!("01234567-1234-4234-8234-{:012x}", i + 32);
            assert_eq!(
                session_name("Claude", "/git/a project", &sid),
                session_name("Claude", "/git/a project", &duplicate)
            );
        }
        let sid = new_sid().unwrap();
        super::super::validate::assert_valid_sid(&sid).unwrap();
        assert_eq!(&sid[14..15], "4");
        assert!(!ANIMALS.contains(&"otter") && !ANIMALS.contains(&"hawk"));
    }
}
