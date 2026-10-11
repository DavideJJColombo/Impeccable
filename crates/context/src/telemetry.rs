//! The roll API's client side, shared by `concept-seed` (the roll and the
//! agent-run choice ping) and `serve-question` (the picker's own ping).
//!
//! Three pieces live here:
//! - the request plumbing: API base, timeout, opt-outs, and the
//!   `X-Impeccable-Client` header every `/api/roll` and `/api/chosen` request
//!   carries;
//! - the seed ledger, `.impeccable/questions/seeds/<tag>.json`: one file per
//!   seed key, one entry per API-dealt round (`poolRevision`, the dealt catalog
//!   ids), plus the marker that a round's choice was already sent. Unlike
//!   `roll.json`, which a decision page removes when it takes the roll, the
//!   ledger outlives the page, so the ping the agent sends after the round
//!   still finds its round;
//! - the choice ping body and its send.

use crate::jsp;
use crate::util::{json_compact, now_ms, safe_read, Env};
use serde_json::{json, Map, Value};
use std::time::Duration;

pub const KINDS: [&str; 4] = ["assigned", "pick", "challenger", "canon"];

/// The highest re-roll round the choice ping reports.
pub const MAX_REROLL: usize = 8;

pub fn api_base(env: &Env) -> String {
    let base = env.get("IMPECCABLE_API_URL").filter(|v| !v.is_empty()).cloned().unwrap_or_else(|| "https://impeccable.style/api".to_string());
    base.strip_suffix('/').unwrap_or(&base).to_string()
}

/// `IMPECCABLE_API_TIMEOUT` in ms (default 4000): one budget for the API
/// calls a run makes.
pub fn api_timeout(env: &Env) -> Duration {
    let ms = env.get("IMPECCABLE_API_TIMEOUT").filter(|v| !v.is_empty()).map(|v| crate::critique_storage::js_number(v)).unwrap_or(4000.0);
    let ms = if ms.is_nan() { 0.0 } else { ms.max(0.0) };
    Duration::from_millis(ms as u64)
}

pub fn telemetry_disabled(env: &Env) -> bool {
    env.get("IMPECCABLE_NO_TELEMETRY").map(|v| !v.is_empty()).unwrap_or(false) || env.get("DO_NOT_TRACK").map(|v| !v.is_empty()).unwrap_or(false)
}

pub fn agent(timeout: Duration) -> ureq::Agent {
    crate::http::agent_builder().timeout_connect(timeout).timeout(timeout).build()
}

pub const CLIENT_HEADER: &str = "X-Impeccable-Client";

fn version_token(v: &str) -> bool {
    !v.is_empty() && v.len() <= 32 && v.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'+' | b'-'))
}

/// `impeccable/<version>[ <channel>]`, matching
/// `^impeccable/[0-9A-Za-z.+-]{1,32}( [a-z]{1,16})?$`. The version is the
/// installed skill's (SKILL.md frontmatter, what `/api/version` compares)
/// when the engine can read it, else the engine's own as `<engine>+engine`.
/// The channel is `IMPECCABLE_TELEMETRY_CHANNEL` when it is 1 to 16 lowercase
/// letters (`test` in this repo's tests and CI), else absent.
pub fn client_header(env: &Env, cwd: &str) -> String {
    let skill = crate::context_cli::read_local_skill_version(&crate::provider::detect(env, cwd)).filter(|v| version_token(v));
    let version = skill.unwrap_or_else(|| format!("{}+engine", env!("CARGO_PKG_VERSION")));
    let channel = env
        .get("IMPECCABLE_TELEMETRY_CHANNEL")
        .filter(|c| !c.is_empty() && c.len() <= 16 && c.bytes().all(|b| b.is_ascii_lowercase()))
        .map(|c| format!(" {}", c))
        .unwrap_or_default();
    format!("impeccable/{}{}", version, channel)
}

/// A roll response's `poolRevision` when it is the 12 lowercase hex the API
/// sends; anything else is not recorded or forwarded.
pub fn valid_pool_revision(v: &str) -> bool {
    v.len() == 12 && v.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

// ---- seed ledger -------------------------------------------------------------

/// One API-dealt round as concept-seed recorded it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SeedRound {
    pub scope: String,
    pub reroll: usize,
    pub mode: Option<String>,
    pub register: Option<String>,
    pub pool_revision: Option<String>,
    /// The catalog ids the round printed as challengers, in print order.
    pub dealt: Vec<String>,
    /// `picker` or `agent` once that round's choice was sent.
    pub recorded: Option<String>,
}

fn seeds_dir(cwd: &str) -> String {
    jsp::join(&[cwd, ".impeccable", "questions", "seeds"])
}

/// The key as a file name: itself when it is a short run of `[a-z0-9_-]`
/// (every key the engine generates), else a hash, so a key can never name a
/// path, and two keys differing only in case never share a file.
fn seed_tag(key: &str) -> String {
    if !key.is_empty() && key.len() <= 64 && key.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-') {
        key.to_string()
    } else {
        use sha2::{Digest, Sha256};
        Sha256::digest(key.as_bytes()).iter().take(8).map(|b| format!("{:02x}", b)).collect()
    }
}

fn seed_file(cwd: &str, key: &str) -> String {
    jsp::join(&[&seeds_dir(cwd), &format!("{}.json", seed_tag(key))])
}

/// Ledger files older than this are pruned when a new one is written.
const SEED_TTL_MS: f64 = 30.0 * 24.0 * 60.0 * 60.0 * 1000.0;

fn read_ledger(cwd: &str, key: &str) -> Option<Map<String, Value>> {
    let m = safe_read(&seed_file(cwd, key)).and_then(|t| serde_json::from_str::<Value>(&t).ok()).and_then(|v| v.as_object().cloned())?;
    // A hashed tag could in principle collide; the stored key settles it.
    (m.get("key").and_then(Value::as_str) == Some(key)).then_some(m)
}

fn write_ledger(cwd: &str, key: &str, ledger: &Map<String, Value>) {
    let dir = seeds_dir(cwd);
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let target = seed_file(cwd, key);
    let temp = format!("{}.tmp-{}", target, std::process::id());
    if std::fs::write(&temp, json_compact(&Value::Object(ledger.clone()))).and_then(|_| std::fs::rename(&temp, &target)).is_err() {
        let _ = std::fs::remove_file(&temp);
    }
}

fn round_from(v: &Value, reroll: usize) -> SeedRound {
    let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    SeedRound {
        scope: s("scope").unwrap_or_default(),
        reroll,
        mode: s("mode"),
        register: s("register"),
        pool_revision: s("poolRevision").filter(|p| valid_pool_revision(p)),
        dealt: v.get("dealt").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default(),
        recorded: s("recorded"),
    }
}

/// Records an API-dealt round under its seed key, replacing an earlier
/// record of the same round (a reproduced roll clears its sent marker, since
/// it is a new presentation). Best effort, like the roll record.
pub fn record_seed_round(cwd: &str, key: &str, round: &SeedRound) {
    let mut ledger = read_ledger(cwd, key).unwrap_or_default();
    ledger.insert("key".into(), Value::String(key.to_string()));
    let mut rounds = ledger.get("rounds").and_then(Value::as_object).cloned().unwrap_or_default();
    let mut r = Map::new();
    r.insert("scope".into(), Value::String(round.scope.clone()));
    if let Some(m) = &round.mode {
        r.insert("mode".into(), Value::String(m.clone()));
    }
    if let Some(reg) = &round.register {
        r.insert("register".into(), Value::String(reg.clone()));
    }
    if let Some(p) = round.pool_revision.as_ref().filter(|p| valid_pool_revision(p)) {
        r.insert("poolRevision".into(), Value::String(p.clone()));
    }
    r.insert("dealt".into(), json!(round.dealt));
    r.insert("at".into(), Value::from(now_ms() as u64));
    rounds.insert(round.reroll.to_string(), Value::Object(r));
    ledger.insert("rounds".into(), Value::Object(rounds));
    write_ledger(cwd, key, &ledger);
    prune_ledgers(cwd);
}

fn prune_ledgers(cwd: &str) {
    let Ok(entries) = std::fs::read_dir(seeds_dir(cwd)) else { return };
    for e in entries.flatten() {
        let old = e.metadata().ok().and_then(|m| m.modified().ok()).and_then(|t| t.elapsed().ok()).is_some_and(|age| age.as_millis() as f64 > SEED_TTL_MS);
        if old {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// The recorded round `reroll` for `key`, or the latest recorded round when
/// `reroll` is `None` (a choice resolves on the round last dealt).
pub fn read_seed_round(cwd: &str, key: &str, reroll: Option<usize>) -> Option<SeedRound> {
    let ledger = read_ledger(cwd, key)?;
    let rounds = ledger.get("rounds")?.as_object()?;
    let n = match reroll {
        Some(n) => n,
        None => rounds.keys().filter_map(|k| k.parse::<usize>().ok()).max()?,
    };
    rounds.get(&n.to_string()).map(|v| round_from(v, n))
}

/// Marks a round's choice as sent by `by`. Returns false when the round is
/// not recorded or was already marked (by anyone).
pub fn mark_recorded(cwd: &str, key: &str, reroll: usize, by: &str) -> bool {
    let Some(mut ledger) = read_ledger(cwd, key) else { return false };
    let Some(Value::Object(rounds)) = ledger.get_mut("rounds") else { return false };
    let Some(Value::Object(r)) = rounds.get_mut(&reroll.to_string()) else { return false };
    if r.contains_key("recorded") {
        return false;
    }
    r.insert("recorded".into(), Value::String(by.to_string()));
    write_ledger(cwd, key, &ledger);
    true
}

/// Clears a round's sent marker when `by` set it: a ping that failed to
/// leave the machine, so the agent's own ping may still send it.
pub fn unmark_recorded(cwd: &str, key: &str, reroll: usize, by: &str) {
    let Some(mut ledger) = read_ledger(cwd, key) else { return };
    let Some(Value::Object(rounds)) = ledger.get_mut("rounds") else { return };
    let Some(Value::Object(r)) = rounds.get_mut(&reroll.to_string()) else { return };
    if r.get("recorded").and_then(Value::as_str) == Some(by) {
        r.remove("recorded");
        write_ledger(cwd, key, &ledger);
    }
}

// ---- the choice ping ---------------------------------------------------------

/// One `/api/chosen` body. `source` is `agent` for the agent-run `--kind`
/// command and `picker` when the decision page's server sends it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ChoicePing {
    pub chosen_id: Option<String>,
    pub key: Option<String>,
    pub scope: Option<String>,
    pub mode: Option<String>,
    pub kind: Option<String>,
    pub register: Option<String>,
    pub reroll: Option<usize>,
    pub pool_revision: Option<String>,
    pub source: &'static str,
}

impl ChoicePing {
    /// The JSON body, keys in a fixed order, absent values omitted.
    pub fn body(&self) -> Value {
        let mut body = Map::new();
        let mut put = |k: &str, v: &Option<String>| {
            if let Some(v) = v {
                body.insert(k.into(), Value::String(v.clone()));
            }
        };
        put("chosenId", &self.chosen_id);
        put("key", &self.key);
        put("scope", &self.scope);
        put("mode", &self.mode);
        put("kind", &self.kind);
        put("register", &self.register);
        if let Some(r) = self.reroll.filter(|r| *r <= MAX_REROLL) {
            body.insert("reroll".into(), Value::from(r));
        }
        if let Some(p) = self.pool_revision.as_ref().filter(|p| valid_pool_revision(p)) {
            body.insert("poolRevision".into(), Value::String(p.clone()));
        }
        body.insert("source".into(), Value::String(self.source.to_string()));
        Value::Object(body)
    }
}

/// POSTs the ping to `/api/chosen` within `timeout`. Never fails the
/// caller: any error is a false.
pub fn send_choice(env: &Env, cwd: &str, ping: &ChoicePing, timeout: Duration) -> bool {
    if timeout.is_zero() {
        return false;
    }
    let url = format!("{}/chosen", api_base(env));
    agent(timeout)
        .post(&url)
        .set("Content-Type", "application/json")
        .set(CLIENT_HEADER, &client_header(env, cwd))
        .send_string(&json_compact(&ping.body()))
        .is_ok()
}

// ---- the picker's choice -----------------------------------------------------

fn card_text<'a>(card: &'a Value, k: &str) -> Option<&'a str> {
    card.get(k).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
}

fn is_pick_kicker(kicker: &str) -> bool {
    let k = kicker.to_uppercase().replace('\u{2019}', "'");
    k == "IMPECCABLE'S PICK"
}

/// The single dealt catalog world a challenger card draws from, matched by
/// its card images: the same board or hero URL the roll printed, or the
/// workspace copy serve-question keeps of it (`<id>-board.<ext>` /
/// `<id>-hero.<ext>`). `None` when no world or more than one matches.
fn matched_world(card: &Value, dealt: &[Value]) -> Option<String> {
    let images: Vec<&str> = ["hero", "board"].iter().filter_map(|k| card_text(card, k)).collect();
    let mut found: Vec<String> = Vec::new();
    for w in dealt {
        let Some(id) = w.get("id").and_then(Value::as_str) else { continue };
        let urls: Vec<&str> = ["hero", "board"].iter().filter_map(|k| w.get(*k).and_then(Value::as_str)).collect();
        let hit = images.iter().any(|img| {
            if urls.contains(img) {
                return true;
            }
            let file = img.replace('\\', "/");
            let name = file.rsplit('/').next().unwrap_or("");
            let stem = name.rsplit_once('.').map(|(s, _)| s).unwrap_or(name);
            !img.contains("://") && (stem == format!("{}-hero", id) || stem == format!("{}-board", id))
        });
        if hit && !found.iter().any(|f| f == id) {
            found.push(id.to_string());
        }
    }
    (found.len() == 1).then(|| found.remove(0))
}

/// The choice ping for an answer on a decision page, when the engine can
/// tell the card's kind without guessing. `seed` is the hand's `seed` record
/// (an API-dealt direction roll the page took); `payload` is the round the
/// page served; `option_id` the answer's.
///
/// - `canon`: the reserved option id.
/// - `challenger`: the card carries a `verdict`, which only challengers do,
///   and its card images match exactly one world the roll dealt; that
///   world's catalog id is the `chosenId`. A verdict card with no match is
///   left to the agent.
/// - `pick`: kicker `IMPECCABLE'S PICK`, the kicker convention new-work.md
///   gives the pick card.
/// - `assigned`: kicker `THE ROLL`, or the lead card (new-work.md puts the
///   assigned direction first) with no verdict and no pick kicker.
///
/// `pick` and `assigned` are only read on a plain round: a register round
/// (safer or bolder) has no assigned card, so its lead means nothing. Every
/// other answer (re-roll, surface rounds, a card none of these fit) gets
/// `None`, and the agent's own ping stays the channel.
pub fn picker_choice(seed: &Value, payload: &Value, option_id: &str) -> Option<ChoicePing> {
    if seed.get("source").and_then(Value::as_str) != Some("api") || seed.get("scope").and_then(Value::as_str) != Some("direction") {
        return None;
    }
    let key = seed.get("key").and_then(Value::as_str).filter(|k| !k.is_empty())?;
    let reroll = seed.get("reroll").and_then(Value::as_u64).map(|r| r as usize).filter(|r| *r <= MAX_REROLL)?;
    let register = seed.get("register").and_then(Value::as_str).filter(|r| *r == "safer" || *r == "bolder");
    let dealt: Vec<Value> = seed.get("dealt").and_then(Value::as_array).cloned().unwrap_or_default();
    let options: Vec<Value> = payload.get("options").and_then(Value::as_array).cloned().unwrap_or_default();

    let (kind, chosen_id) = if option_id == "canon" {
        ("canon", None)
    } else if option_id.is_empty() || option_id == "reroll" {
        return None;
    } else {
        let (index, card) = options.iter().enumerate().find(|(_, o)| o.get("id").and_then(Value::as_str) == Some(option_id))?;
        let verdict = card_text(card, "verdict");
        let kicker = card_text(card, "kicker");
        if verdict.is_some() {
            ("challenger", Some(matched_world(card, &dealt)?))
        } else if register.is_some() {
            return None;
        } else if kicker.is_some_and(is_pick_kicker) {
            ("pick", None)
        } else if kicker.is_some_and(|k| k.eq_ignore_ascii_case("THE ROLL")) || index == 0 {
            ("assigned", None)
        } else {
            return None;
        }
    };
    let s = |k: &str| seed.get(k).and_then(Value::as_str).map(str::to_string);
    Some(ChoicePing {
        chosen_id,
        key: Some(key.to_string()),
        scope: Some("direction".into()),
        mode: s("mode"),
        kind: Some(kind.into()),
        register: register.map(str::to_string),
        reroll: Some(reroll),
        pool_revision: s("poolRevision").filter(|p| valid_pool_revision(p)),
        source: "picker",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> String {
        let dir = std::env::temp_dir().join(format!("impeccable-telemetry-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.to_string_lossy().into_owned()
    }

    fn env(pairs: &[(&str, &str)]) -> Env {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    fn header_ok(h: &str) -> bool {
        let re = regex::Regex::new(r"^impeccable/[0-9A-Za-z.+-]{1,32}( [a-z]{1,16})?$").unwrap();
        re.is_match(h)
    }

    #[test]
    fn client_header_names_the_skill_version_else_the_engine_and_the_channel() {
        let dir = temp_dir("header");
        let skill = format!("{}/skill", dir);
        std::fs::create_dir_all(&skill).unwrap();
        let e = env(&[("IMPECCABLE_SKILL_DIR", &skill)]);
        let h = client_header(&e, &dir);
        assert_eq!(h, format!("impeccable/{}+engine", env!("CARGO_PKG_VERSION")));
        assert!(header_ok(&h), "{h}");
        std::fs::write(format!("{}/SKILL.md", skill), "---\nname: impeccable\nversion: 4.5.2\n---\n# x\n").unwrap();
        assert_eq!(client_header(&e, &dir), "impeccable/4.5.2");
        let e = env(&[("IMPECCABLE_SKILL_DIR", &skill), ("IMPECCABLE_TELEMETRY_CHANNEL", "test")]);
        let h = client_header(&e, &dir);
        assert_eq!(h, "impeccable/4.5.2 test");
        assert!(header_ok(&h), "{h}");
        // A channel that does not fit the format is dropped, never sent malformed.
        for bad in ["Test", "te st", "abcdefghijklmnopq", "t3st"] {
            let e = env(&[("IMPECCABLE_SKILL_DIR", &skill), ("IMPECCABLE_TELEMETRY_CHANNEL", bad)]);
            assert_eq!(client_header(&e, &dir), "impeccable/4.5.2", "{bad}");
        }
        // A frontmatter version outside the token alphabet falls back to the engine.
        std::fs::write(format!("{}/SKILL.md", skill), "---\nversion: 4.5 beta\n---\n").unwrap();
        assert!(client_header(&env(&[("IMPECCABLE_SKILL_DIR", &skill)]), &dir).ends_with("+engine"));
    }

    #[test]
    fn ledger_records_rounds_marks_once_and_unmarks_only_its_own() {
        let cwd = temp_dir("ledger");
        let round = |n: usize, dealt: &[&str]| SeedRound {
            scope: "direction".into(),
            reroll: n,
            mode: Some("persuade".into()),
            register: None,
            pool_revision: Some("0123456789ab".into()),
            dealt: dealt.iter().map(|s| s.to_string()).collect(),
            recorded: None,
        };
        assert_eq!(read_seed_round(&cwd, "k1", None), None);
        record_seed_round(&cwd, "k1", &round(0, &["a", "b"]));
        record_seed_round(&cwd, "k1", &round(1, &["c"]));
        assert_eq!(read_seed_round(&cwd, "k1", None).unwrap().dealt, vec!["c"]);
        assert_eq!(read_seed_round(&cwd, "k1", Some(0)).unwrap(), round(0, &["a", "b"]));
        assert_eq!(read_seed_round(&cwd, "k1", Some(5)), None);
        assert!(mark_recorded(&cwd, "k1", 0, "picker"));
        assert!(!mark_recorded(&cwd, "k1", 0, "agent"));
        assert_eq!(read_seed_round(&cwd, "k1", Some(0)).unwrap().recorded.as_deref(), Some("picker"));
        unmark_recorded(&cwd, "k1", 0, "agent");
        assert!(read_seed_round(&cwd, "k1", Some(0)).unwrap().recorded.is_some());
        unmark_recorded(&cwd, "k1", 0, "picker");
        assert!(read_seed_round(&cwd, "k1", Some(0)).unwrap().recorded.is_none());
        // A key that is not a plain slug never names a path.
        record_seed_round(&cwd, "../Evil Key", &round(0, &["x"]));
        assert_eq!(read_seed_round(&cwd, "../Evil Key", None).unwrap().dealt, vec!["x"]);
        assert!(std::fs::read_dir(format!("{}/.impeccable/questions/seeds", cwd)).unwrap().flatten().all(|e| !e.file_name().to_string_lossy().contains("..")));
        // A malformed poolRevision is never recorded.
        let mut r = round(2, &[]);
        r.pool_revision = Some("NOT-HEX".into());
        record_seed_round(&cwd, "k1", &r);
        assert_eq!(read_seed_round(&cwd, "k1", Some(2)).unwrap().pool_revision, None);
    }

    #[test]
    fn body_orders_fields_and_drops_out_of_range_values() {
        let p = ChoicePing {
            chosen_id: Some("w1".into()),
            key: Some("k".into()),
            scope: Some("direction".into()),
            mode: None,
            kind: Some("challenger".into()),
            register: Some("bolder".into()),
            reroll: Some(2),
            pool_revision: Some("0123456789ab".into()),
            source: "agent",
        };
        assert_eq!(json_compact(&p.body()), r#"{"chosenId":"w1","key":"k","scope":"direction","kind":"challenger","register":"bolder","reroll":2,"poolRevision":"0123456789ab","source":"agent"}"#);
        let p = ChoicePing { reroll: Some(9), pool_revision: Some("xyz".into()), source: "picker", ..Default::default() };
        assert_eq!(json_compact(&p.body()), r#"{"source":"picker"}"#);
    }

    fn seed(register: Option<&str>) -> Value {
        let mut s = json!({
            "key": "k1", "scope": "direction", "reroll": 1, "mode": "persuade", "source": "api", "poolRevision": "0123456789ab",
            "dealt": [
                { "id": "teletext-service", "board": "https://cards.test/teletext-service.webp", "hero": "https://cards.test/teletext-service-hero.webp" },
                { "id": "microfiche-reader", "board": "https://cards.test/microfiche-reader.webp", "hero": "https://cards.test/microfiche-reader-hero.webp" }
            ]
        });
        if let Some(r) = register {
            s["register"] = json!(r);
        }
        s
    }

    fn page() -> Value {
        json!({ "options": [
            { "id": "assigned", "label": "Fillmore", "hero": "https://cards.test/teletext-service-hero.webp" },
            { "id": "model-pick", "label": "Broadside", "kicker": "IMPECCABLE’S PICK" },
            { "id": "challenger-teletext", "verdict": "competitive", "hero": "https://cards.test/teletext-service-hero.webp" },
            { "id": "challenger-microfiche", "verdict": "declined", "hero": ".impeccable/mocks/worlds/microfiche-reader-hero.webp" },
            { "id": "challenger-own", "verdict": "wins", "hero": "https://elsewhere.test/x.webp" },
            { "id": "grounded-2", "label": "Another" }
        ], "canon": true })
    }

    fn kind_and_id(p: Option<ChoicePing>) -> Option<(String, Option<String>)> {
        p.map(|p| (p.kind.unwrap(), p.chosen_id))
    }

    #[test]
    fn picker_reads_kinds_only_where_the_page_says_them() {
        let s = seed(None);
        let pg = page();
        let k = |id: &str| kind_and_id(picker_choice(&s, &pg, id));
        // The assigned card may wear a catalog image; it is still the assigned card, with no id.
        assert_eq!(k("assigned"), Some(("assigned".into(), None)));
        assert_eq!(k("model-pick"), Some(("pick".into(), None)));
        assert_eq!(k("challenger-teletext"), Some(("challenger".into(), Some("teletext-service".into()))));
        assert_eq!(k("challenger-microfiche"), Some(("challenger".into(), Some("microfiche-reader".into()))), "the workspace copy of a card image matches its world");
        assert_eq!(k("canon"), Some(("canon".into(), None)));
        // Never guessed: a challenger with no dealt world, an unlabeled card past the lead, a re-roll, an unknown id.
        assert_eq!(k("challenger-own"), None);
        assert_eq!(k("grounded-2"), None);
        assert_eq!(k("reroll"), None);
        assert_eq!(k("nope"), None);
        let p = picker_choice(&s, &pg, "challenger-teletext").unwrap();
        assert_eq!(json_compact(&p.body()), r#"{"chosenId":"teletext-service","key":"k1","scope":"direction","mode":"persuade","kind":"challenger","reroll":1,"poolRevision":"0123456789ab","source":"picker"}"#);
    }

    #[test]
    fn picker_reads_only_canon_and_matched_challengers_on_register_rounds() {
        let pg = page();
        for r in ["safer", "bolder"] {
            let s = seed(Some(r));
            assert_eq!(kind_and_id(picker_choice(&s, &pg, "assigned")), None, "{r}");
            assert_eq!(kind_and_id(picker_choice(&s, &pg, "model-pick")), None, "{r}");
            assert_eq!(kind_and_id(picker_choice(&s, &pg, "canon")), Some(("canon".into(), None)), "{r}");
            let p = picker_choice(&s, &pg, "challenger-teletext").unwrap();
            assert_eq!(p.register.as_deref(), Some(r));
        }
    }

    #[test]
    fn picker_stays_out_of_surface_and_non_api_rounds() {
        let pg = page();
        let mut s = seed(None);
        s["scope"] = json!("surface");
        assert_eq!(picker_choice(&s, &pg, "assigned"), None);
        let mut s = seed(None);
        s["source"] = json!("local");
        assert_eq!(picker_choice(&s, &pg, "assigned"), None);
        let mut s = seed(None);
        s["reroll"] = json!(9);
        assert_eq!(picker_choice(&s, &pg, "assigned"), None);
        // A world two dealt entries share an image with is ambiguous, so no id and no ping.
        let mut s = seed(None);
        s["dealt"][1]["hero"] = json!("https://cards.test/teletext-service-hero.webp");
        assert_eq!(picker_choice(&s, &pg, "challenger-teletext"), None);
    }
}
