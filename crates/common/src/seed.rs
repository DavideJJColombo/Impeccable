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
//! a reference resolves only on the machine (user) that made it.
//! `IMPECCABLE_SEED_SALT` overrides the salt, for reproducible tests.

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

/// The per-user salt: `IMPECCABLE_SEED_SALT` when set, else
/// `<home>/.impeccable/seed-salt`, created on first use (create-new, so two
/// processes never write different salts). Empty when no home is writable,
/// which leaves references unsalted rather than failing a roll.
pub fn salt(home: &str, override_salt: Option<&str>) -> String {
    if let Some(s) = override_salt.filter(|s| !s.is_empty()) {
        return s.to_string();
    }
    if home.is_empty() {
        return String::new();
    }
    let dir = std::path::Path::new(home).join(".impeccable");
    let file = dir.join("seed-salt");
    let read = || std::fs::read_to_string(&file).ok().map(|s| s.trim().to_string()).filter(|s| is_hex(s, 32));
    if let Some(s) = read() {
        return s;
    }
    if let Some(fresh) = random_hex32() {
        let _ = std::fs::create_dir_all(&dir);
        if let Ok(mut f) = std::fs::OpenOptions::new().write(true).create_new(true).open(&file) {
            use std::io::Write;
            let _ = f.write_all(format!("{}\n", fresh).as_bytes());
        }
    }
    read().unwrap_or_default()
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

    #[test]
    fn the_salt_is_created_once_per_home_and_can_be_pinned() {
        let home = std::env::temp_dir().join(format!("impeccable-salt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let h = home.to_string_lossy().into_owned();
        let first = salt(&h, None);
        assert!(is_hex(&first, 32), "{first}");
        assert_eq!(salt(&h, None), first, "stable");
        assert_eq!(salt(&h, Some("pinned")), "pinned");
        assert_eq!(salt("", None), "");
        let _ = std::fs::remove_dir_all(&home);
    }
}
