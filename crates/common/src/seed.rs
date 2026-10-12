//! Seed keys and seed references.
//!
//! A roll key is what the roll API deals from and what the choice ping
//! names, so it must never be committed. Committed files (the surface brief,
//! the build state) carry a seed reference instead: `seed:` plus 16 hex of
//! SHA-256(`impeccable-seed:` + salt + `:` + key). The salt is 128 random
//! bits kept per user in `~/.impeccable/seed-salt`, so a reference cannot be
//! computed from a key by anyone without that file: holding the telemetry
//! dataset (which carries keys) is not enough to find a committed brief, and
//! a short key cannot be enumerated back from its reference. The flip side:
//! a reference resolves only on the machine (user) that made it. When no
//! salt can be persisted there is no reference at all (`salt` is `None`),
//! never an unsalted one. `IMPECCABLE_SEED_SALT` overrides the salt, for
//! reproducible tests.

use sha2::{Digest, Sha256};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

fn is_hex(v: &str, len: usize) -> bool {
    v.len() == len && v.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// 128 bits from the OS CSPRNG as 32 lowercase hex. `None` only when the OS
/// generator fails.
pub fn random_hex32() -> Option<String> {
    let mut b = [0u8; 16];
    getrandom::getrandom(&mut b).ok()?;
    Some(hex(&b))
}

/// A new roll key: 32 lowercase hex from the OS CSPRNG (the roll API takes
/// `^[a-z0-9-]{1,64}$`). Falls back to a clock and pid mix only when the OS
/// generator fails, which it does not on any supported platform.
pub fn new_key() -> String {
    random_hex32().unwrap_or_else(|| {
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        hex(&Sha256::digest(format!("{}:{}", t, std::process::id()).as_bytes())[..16])
    })
}

/// Where a salt is persisted, first that works: the user's
/// `<home>/.impeccable/seed-salt`, else the project's own
/// `<project>/.impeccable/questions/seed-salt` (an ignored runtime folder,
/// for sandboxes whose home is read-only). `IMPECCABLE_SEED_SALT` overrides
/// both, for reproducible tests.
///
/// `None` when no valid salt can be had: a salt file that exists but is not
/// 32 lowercase hex is left alone (rewriting it would silently orphan every
/// reference made under it), and a location that cannot be written yields
/// nothing. There is never an unsalted fallback: an unsalted reference is a
/// public hash of the key, linkable to anyone holding keys.
pub fn salt(home: &str, project: Option<&str>, override_salt: Option<&str>) -> Option<String> {
    if let Some(s) = override_salt.filter(|s| !s.is_empty()) {
        return Some(s.to_string());
    }
    let mut places: Vec<std::path::PathBuf> = Vec::new();
    if !home.is_empty() {
        places.push(std::path::Path::new(home).join(".impeccable").join("seed-salt"));
    }
    if let Some(p) = project.filter(|p| !p.is_empty()) {
        places.push(std::path::Path::new(p).join(".impeccable").join("questions").join("seed-salt"));
    }
    for file in places {
        match std::fs::read_to_string(&file) {
            Ok(text) => {
                let t = text.trim();
                // Present: valid or not, this location decides.
                return is_hex(t, 32).then(|| t.to_string());
            }
            Err(_) if file.exists() => return None,
            Err(_) => {}
        }
        let Some(fresh) = random_hex32() else { return None };
        let dir = file.parent().map(|d| d.to_path_buf()).unwrap_or_default();
        if std::fs::create_dir_all(&dir).is_err() {
            continue;
        }
        if file.to_string_lossy().contains("questions") {
            crate::git::ensure_dir_ignored(&dir.to_string_lossy(), &["*"]);
        }
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&file) {
            Ok(mut f) => {
                use std::io::Write;
                if f.write_all(format!("{}\n", fresh).as_bytes()).is_err() {
                    let _ = std::fs::remove_file(&file);
                    continue;
                }
                return Some(fresh);
            }
            // Another process created it first: read theirs.
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                return std::fs::read_to_string(&file).ok().map(|t| t.trim().to_string()).filter(|t| is_hex(t, 32));
            }
            Err(_) => continue,
        }
    }
    None
}

/// The 16 hex of a key's reference under `salt`.
pub fn tag(salt: &str, key: &str) -> String {
    hex(&Sha256::digest(format!("impeccable-seed:{}:{}", salt, key).as_bytes())[..8])
}

/// `seed:<tag>`.
pub fn reference(salt: &str, key: &str) -> String {
    format!("seed:{}", tag(salt, key))
}

/// The 16 hex of a `seed:<16 lowercase hex>` reference, else `None`.
pub fn parse_reference(v: &str) -> Option<&str> {
    v.strip_prefix("seed:").filter(|h| is_hex(h, 16))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_128_bit_hex_and_references_depend_on_the_salt() {
        let a = new_key();
        assert!(is_hex(&a, 32) && a != new_key(), "{a}");
        let r = reference("s1", "k1");
        assert_eq!(r, reference("s1", "k1"));
        assert_ne!(r, reference("s2", "k1"), "a dataset holder without the salt cannot compute it");
        assert_eq!(parse_reference(&r).map(str::len), Some(16));
        assert_eq!(parse_reference("seed:ABCDEF0123456789"), None);
    }

    fn temp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("impeccable-salt-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn the_salt_is_created_once_per_home_and_can_be_pinned() {
        let home = temp("home");
        let h = home.to_string_lossy().into_owned();
        let first = salt(&h, None, None).unwrap();
        assert!(is_hex(&first, 32), "{first}");
        assert_eq!(salt(&h, None, None).unwrap(), first, "stable");
        assert_eq!(salt(&h, None, Some("pinned")).unwrap(), "pinned");
        assert_eq!(salt("", None, None), None, "no place, no salt");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_corrupt_salt_is_never_replaced_or_bypassed() {
        let home = temp("corrupt");
        let project = temp("corrupt-project");
        std::fs::create_dir_all(home.join(".impeccable")).unwrap();
        std::fs::write(home.join(".impeccable/seed-salt"), "not a salt\n").unwrap();
        assert_eq!(salt(&home.to_string_lossy(), Some(&project.to_string_lossy()), None), None);
        assert_eq!(std::fs::read_to_string(home.join(".impeccable/seed-salt")).unwrap(), "not a salt\n", "left for the user");
        assert!(!project.join(".impeccable/questions/seed-salt").exists(), "no silent switch to another salt");
    }

    #[cfg(unix)]
    #[test]
    fn an_unwritable_home_falls_back_to_the_project_and_then_to_nothing() {
        use std::os::unix::fs::PermissionsExt;
        let home = temp("readonly");
        let project = temp("readonly-project");
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o555)).unwrap();
        if std::fs::write(home.join("probe"), "").is_ok() {
            return; // running as root: permissions are not enforced
        }
        let p = salt(&home.to_string_lossy(), Some(&project.to_string_lossy()), None).unwrap();
        assert_eq!(std::fs::read_to_string(project.join(".impeccable/questions/seed-salt")).unwrap().trim(), p);
        assert!(std::fs::read_to_string(project.join(".impeccable/questions/.gitignore")).unwrap().ends_with("*\n"));
        std::fs::set_permissions(&project, std::fs::Permissions::from_mode(0o555)).unwrap();
        let other = temp("readonly-project-2");
        std::fs::set_permissions(&other, std::fs::Permissions::from_mode(0o555)).unwrap();
        assert_eq!(salt(&home.to_string_lossy(), Some(&other.to_string_lossy()), None), None, "nowhere to keep one");
        for d in [&home, &project, &other] {
            let _ = std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o755));
            let _ = std::fs::remove_dir_all(d);
        }
    }
}
