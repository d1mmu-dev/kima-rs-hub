//! Playlists: the list (with create) and the detail page (rename, delete, remove tracks, play).

pub mod detail;
pub mod list;

/// The name as it is sent: trimmed, 1 to 200 characters (the backend's rule), else a message.
pub fn validate_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    match name.chars().count() {
        0 => Err("Enter a playlist name".into()),
        1..=200 => Ok(name.to_owned()),
        _ => Err("Playlist name must be at most 200 characters".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_trimmed_and_bounded() {
        assert!(validate_name("").is_err());
        assert!(validate_name("   ").is_err());
        assert_eq!(validate_name("  Road trip ").unwrap(), "Road trip");
        assert!(validate_name(&"x".repeat(200)).is_ok());
        assert!(validate_name(&"x".repeat(201)).is_err());
        // Characters, not bytes.
        assert!(validate_name(&"ż".repeat(200)).is_ok());
    }
}
