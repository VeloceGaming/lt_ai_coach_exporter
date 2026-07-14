//! LT AI Coach Exporter.
//!
//! Read-only native mod. When the coach asks (by dropping a request file), it
//! reads the live game Database through the official mod API and writes the data
//! LT AI Coach needs. It exports only on request — so the data is always fresh
//! for the open game and nothing is exported wastefully. It changes no game state.
//!
//! This replaces the borrowed `tfm2_save_probe.exe` and the offline save-file
//! parser: reading the live, already-deserialized Database means the export
//! always matches the current game version, so game updates can't break it the
//! way reverse-engineering the save bytes does.
//!
//! Stage 1 proved the pipeline (champions + teams). Stage 2a (this file) adds
//! players (athletes) and the match-table sizes, confirming we can reach the
//! match data. Stage 2b will extract each match's picks/bans/stats; then patches.
//!
//! Last updated: 2026-06-17. Target: Teamfight Manager 2 mod SDK (mod_api).

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use mod_api::*;

const MOD_ID: &str = "lt_ai_coach_exporter";

// The coach drops this file in the export folder to ask for an export; the mod
// exports the current game and deletes it. Nothing is exported otherwise.
const REQUEST_FILE: &str = "request.txt";
const PORTRAIT_REQUEST_FILE: &str = "portrait_request.txt";

struct ExportTrace {
    started: Instant,
    previous: Instant,
    rows: Vec<(&'static str, u128)>,
}

impl ExportTrace {
    fn new() -> Self {
        let now = Instant::now();
        Self {
            started: now,
            previous: now,
            rows: Vec::new(),
        }
    }

    fn mark(&mut self, stage: &'static str) {
        let now = Instant::now();
        self.rows
            .push((stage, now.duration_since(self.previous).as_micros()));
        self.previous = now;
    }

    fn finish(mut self) {
        self.rows
            .push(("export_total", self.started.elapsed().as_micros()));
        let mut output = String::from("stage\tduration_us\n");
        for (stage, duration_us) in self.rows {
            output.push_str(&format!("{stage}\t{duration_us}\n"));
        }
        write_file("performance_export.tsv", &output);
    }
}

fn performance_enabled() -> bool {
    std::env::var("LOCALAPPDATA")
        .ok()
        .map(|root| Path::new(&root).join("com.lttools.lt-ai-coach/performance.enabled"))
        .is_some_and(|path| path.is_file())
}

fn mark_trace(trace: &mut Option<ExportTrace>, stage: &'static str) {
    if let Some(trace) = trace {
        trace.mark(stage);
    }
}

#[derive(Default)]
struct ExporterExtension;

impl ModExtension for ExporterExtension {
    fn post_update(&self, scene: &mut Scene, _ui: &mut GameUI, assets: &mut Assets, _dt: f32) {
        // Only act when a game is actually loaded.
        let Scene::InGame { data } = scene else {
            return;
        };
        if portrait_export_requested() {
            export_portrait_probe(data, assets);
            clear_named_request(PORTRAIT_REQUEST_FILE);
            return;
        }
        // Export only when the coach asks (on "Import from Game"). This way the
        // data is always fresh for the open game and we never export wastefully.
        if !export_requested() {
            return;
        }
        export_all(data);
        clear_request();
    }
}

/// The LT AI Coach data folder, where the coach drops its request and reads the
/// export (`%LOCALAPPDATA%/com.lttools.lt-ai-coach/exporter`).
fn coach_export_dir() -> Option<String> {
    std::env::var("LOCALAPPDATA")
        .ok()
        .map(|local_app_data| format!("{local_app_data}/com.lttools.lt-ai-coach/exporter"))
}

/// True if the coach has asked for an export.
fn export_requested() -> bool {
    coach_export_dir()
        .map(|dir| Path::new(&format!("{dir}/{REQUEST_FILE}")).is_file())
        .unwrap_or(false)
}

/// Remove the request once handled, so we export exactly once per click.
fn clear_request() {
    clear_named_request(REQUEST_FILE);
}

fn portrait_export_requested() -> bool {
    coach_export_dir()
        .map(|dir| Path::new(&format!("{dir}/{PORTRAIT_REQUEST_FILE}")).is_file())
        .unwrap_or(false)
}

fn clear_named_request(file: &str) {
    if let Some(dir) = coach_export_dir() {
        let _ = fs::remove_file(format!("{dir}/{file}"));
    }
}

fn export_portrait_probe(data: &ClientData, assets: &Assets) {
    let db = data.db();
    let sheet = serde_json::to_value(&db.champion_info_sheet).unwrap_or_default();
    let mut mod_sprites = BTreeMap::new();
    if let Some(entries) = sheet.get("mod_champions").and_then(|value| value.as_array()) {
        for entry in entries {
            if let (Some(id), Some(sprite)) = (
                entry.get("id").and_then(|value| value.as_str()),
                entry.get("sprite").and_then(|value| value.as_str()),
            ) {
                mod_sprites.insert(id.to_string(), sprite.to_string());
            }
        }
    }
    let mut results = Vec::new();
    for champion_id in &db.available_champions {
        let asset = mod_sprites.get(champion_id).cloned().unwrap_or_else(|| {
            format!("asset/base/aseprite_resources/champions/{champion_id}")
        });
        let animation_asset = format!("{asset}#anim");
        let sheet_asset = format!("{asset}#sheet");

        // Aseprite's loader consumes the source Ase and registers these two
        // synthetic assets. The raw Ase therefore normally no longer exists in
        // the live registry, but the exact rectangles used by the game do.
        let animations = assets
            .get::<engine_core::animation::RectAnimationSet>(&animation_asset);
        let sheet_png = assets
            .get_raw(&sheet_asset)
            .and_then(|loaded| loaded.merge_raw.as_deref())
            .filter(|bytes| bytes.starts_with(b"\x89PNG\r\n\x1a\n"));

        // Keep support for SDK/game versions which retain the source asset.
        let retained_animation = assets
            .get::<engine_asset::ase::Ase>(&asset)
            .and_then(|ase| ase.get_animation());
        // Workshop Aseprite sources remain on disk and the loaded asset keeps
        // that exact path, even though its parsed Ase object is discarded after
        // producing #sheet/#anim. Reparse only when the user asks for repair.
        let file_animation = assets
            .get_raw(&asset)
            .and_then(|loaded| loaded.file_path.as_deref())
            .and_then(|path| fs::read(path).ok())
            .and_then(|bytes| engine_asset::ase::Ase::from_data(&bytes))
            .and_then(|ase| ase.get_animation());
        let source_animation = retained_animation.or(file_animation);
        let names = mod_champion_names(assets, &asset, champion_id);
        let (png, animation_json) = if let Some((png, animations)) = source_animation {
            (Some(png), serde_json::to_value(&animations).unwrap_or_default())
        } else {
            (
                sheet_png.map(ToOwned::to_owned),
                animations
                    .and_then(|value| serde_json::to_value(value).ok())
                    .unwrap_or_default(),
            )
        };

        let related_assets: Vec<serde_json::Value> = assets
            .get_all_assets()
            .into_iter()
            .filter(|(name, _)| name.as_str() == asset || name.starts_with(&format!("{asset}#")))
            .map(|(name, loaded)| {
                serde_json::json!({
                    "name": name,
                    "tag": loaded.tag,
                    "filePath": loaded.file_path,
                    "rawBytes": loaded.merge_raw.as_ref().map_or(0, Vec::len),
                })
            })
            .collect();

        let Some(png) = png else {
            results.push(serde_json::json!({
                "championId": champion_id,
                "asset": asset,
                "status": if animations.is_some() { "animation-only" } else { "not-resolved" },
                "animationAsset": animation_asset,
                "sheetAsset": sheet_asset,
                "relatedAssets": related_assets,
                "names": names,
            }));
            continue;
        };
        if animation_json.is_null() {
            results.push(serde_json::json!({
                "championId": champion_id,
                "asset": asset,
                "status": "sheet-only",
                "pngBytes": png.len(),
                "relatedAssets": related_assets,
                "names": names,
            }));
            continue;
        }
        let png_file = format!("portrait_probe/{champion_id}.png");
        write_bytes(&png_file, &png);
        results.push(serde_json::json!({
            "championId": champion_id,
            "asset": asset,
            "status": "resolved",
            "pngFile": png_file,
            "pngBytes": png.len(),
            "animations": animation_json,
            "names": names,
        }));
    }
    let payload = serde_json::json!({
        "schemaVersion": 2,
        "champions": results,
    });
    write_file("portrait_probe.json", &serde_json::to_string_pretty(&payload).unwrap_or_default());
}

fn mod_champion_names(
    assets: &Assets,
    asset: &str,
    champion_id: &str,
) -> BTreeMap<String, String> {
    let Some(source_path) = assets
        .get_raw(asset)
        .and_then(|loaded| loaded.file_path.as_deref())
    else {
        return BTreeMap::new();
    };
    let i18n_path = Path::new(source_path)
        .ancestors()
        .map(|ancestor| ancestor.join("text/champion.i18n"))
        .find(|candidate| candidate.is_file());
    let Some(document) = i18n_path
        .and_then(|path| fs::read_to_string(path).ok())
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
    else {
        return BTreeMap::new();
    };
    document
        .as_object()
        .into_iter()
        .flat_map(|languages| languages.iter())
        .filter_map(|(language, value)| {
            value
                .pointer(&format!("/description/{champion_id}/name"))
                .and_then(|name| name.as_str())
                .filter(|name| !name.trim().is_empty())
                .map(|name| (language.clone(), name.to_string()))
        })
        .collect()
}

/// Stage 1 export: enabled champions and the team list.
fn export_all(data: &ClientData) {
    let mut trace = performance_enabled().then(ExportTrace::new);
    let db = data.db();
    let save_id = db.id;
    let player_team_id = data.player_team_id();
    // ClientDatabase.id can be reused by independent campaigns. Include the
    // player team and the campaign's earliest persistent match in the ledger
    // identity so a later save can never donate snapshots to an earlier one.
    let campaign_key = campaign_ledger_key(&db, player_team_id);

    // Capture only on an explicit import request. During save loading the game
    // briefly reports boot versions (for example 1970.0.0 and the save's first
    // patch) alongside partially initialized data; capturing every frame turns
    // those transients into enormous fake balance patches.
    capture_current_balance_snapshot(&db, &campaign_key);
    mark_trace(&mut trace, "capture_balance_snapshot");

    // Enabled champions — one id per line.
    let champions = db.available_champions.join("\n");
    write_file("champions.tsv", &(champions + "\n"));
    mark_trace(&mut trace, "write_champions");

    // Champion metadata comes from the user's currently loaded game, including
    // Workshop champions. The game owns category/raw tags; LT AI Coach derives
    // its lightweight role prior from those fields instead of redistributing a
    // third-party catalog.
    let champion_metadata = champion_metadata_export(&db);
    let champion_metadata_json =
        serde_json::to_string(&champion_metadata).unwrap_or_else(|_| "null".to_string());
    mark_trace(&mut trace, "serialize_champion_metadata");
    write_file("champion_metadata.json", &champion_metadata_json);
    mark_trace(&mut trace, "write_champion_metadata");

    // Teams — id<TAB>name, one per line, with a header row.
    let mut team_rows = vec!["id\tname".to_string()];
    for team_id in data.team_ids() {
        let Some(team) = data.team(team_id) else {
            continue;
        };
        team_rows.push(format!("{}\t{}", team.id, sanitize(&team.name)));
    }
    let team_count = team_rows.len() - 1;
    write_file("teams.tsv", &(team_rows.join("\n") + "\n"));
    mark_trace(&mut trace, "write_teams");

    // Players (athletes) — id<TAB>name. Same accessor pattern the athlete_editor
    // mod uses, so this is known-good against the live database.
    let mut player_rows = vec!["id\tname".to_string()];
    for athlete_id in data.athlete_ids() {
        let Some(athlete) = data.athlete(athlete_id) else {
            continue;
        };
        player_rows.push(format!("{}\t{}", athlete.id, sanitize(&athlete.name)));
    }
    let player_count = player_rows.len() - 1;
    write_file("players.tsv", &(player_rows.join("\n") + "\n"));
    mark_trace(&mut trace, "write_players");

    // Athlete champion proficiency/mastery. The new game versions store this
    // on the athlete, separate from basic stat fields.
    let athlete_mastery = athlete_mastery_export(data);
    let athlete_mastery_json =
        serde_json::to_string(&athlete_mastery).unwrap_or_else(|_| "null".to_string());
    mark_trace(&mut trace, "serialize_athletes");
    write_file("athlete_mastery.json", &athlete_mastery_json);
    mark_trace(&mut trace, "write_athletes");

    // Matches — the data the coach scores on (picks, bans, per-player stats).
    // game_core's types derive serde `Serialize` (the save is bincode of them),
    // so we serialize the whole match tables to JSON in one shot. Reading from
    // live memory means every string is valid UTF-8 — no lossy handling needed.
    let match_replays = db.match_replays.len();
    let solo_rank_matches = db.solo_rank_matches.len();
    // Inline `serde_json::to_string` (no generic helper) so we never have to name
    // the `serde::Serialize` bound — only `serde_json` is an explicit extern.
    let tournament_json =
        serde_json::to_string(&db.match_replays).unwrap_or_else(|_| "null".to_string());
    mark_trace(&mut trace, "serialize_match_replays");
    write_file("match_replays.json", &tournament_json);
    mark_trace(&mut trace, "write_match_replays");
    let solo_json =
        serde_json::to_string(&db.solo_rank_matches).unwrap_or_else(|_| "null".to_string());
    mark_trace(&mut trace, "serialize_solo_matches");
    write_file("solo_rank_matches.json", &solo_json);
    mark_trace(&mut trace, "write_solo_matches");

    // Patch states (per-patch champion balance + available champions). The coach
    // uses this to detect which champions changed between patches. `pre_patch_data`
    // is the Database field the borrowed probe dumped under that same name.
    let patches_json =
        serde_json::to_string(&db.pre_patch_data).unwrap_or_else(|_| "null".to_string());
    mark_trace(&mut trace, "serialize_patch_data");
    write_file("pre_patch_data.json", &patches_json);
    mark_trace(&mut trace, "write_patch_data");

    // Exact champion balance sheets by game version. `pre_patch_data` is not a
    // complete history in every save (some saves retain only their initial
    // snapshot), while the database can resolve the sheet that applied on a
    // specific date. Solo-rank matches give us dated representatives for each
    // version seen by the save; the live sheet is always used for the current
    // version. The coach can diff adjacent snapshots without reverse-engineering
    // champion-specific structs or maintaining a hand-written field list.
    let balance_history = champion_balance_history(&db, &campaign_key);
    mark_trace(&mut trace, "build_balance_history");
    let balance_history_json =
        serde_json::to_string(&balance_history).unwrap_or_else(|_| "null".to_string());
    mark_trace(&mut trace, "serialize_balance_history");
    write_file("champion_balance_history.json", &balance_history_json);
    mark_trace(&mut trace, "write_balance_history");

    // Identify which game this export is from and when it was written, so the
    // coach can show the user exactly what it imported and warn if the data is
    // stale. `save_id` also selects its isolated balance-snapshot directory.
    let player_team = data
        .team(player_team_id)
        .map(|team| sanitize(&team.name))
        .unwrap_or_default();
    let exported_at_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);

    // A small manifest so the coach (and the user) can confirm a fresh export.
    let manifest = format!(
        "mod\t{MOD_ID}\nsave_id\t{save_id}\nsave_id_source\tclient_database_id\ncampaign_key\t{campaign_key}\nplayer_team\t{player_team}\nplayer_team_id\t{player_team_id}\nexported_at_unix\t{exported_at_unix}\n\
         champions\t{}\nteams\t{}\nplayers\t{}\nmatch_replays\t{}\nsolo_rank_matches\t{}\n",
        db.available_champions.len(),
        team_count,
        player_count,
        match_replays,
        solo_rank_matches,
    );
    mark_trace(&mut trace, "prepare_manifest");
    if let Some(trace) = trace {
        trace.finish();
    }
    // Manifest stays last: its fresh timestamp is the coach's signal that all
    // export data and the performance trace are complete.
    write_file("manifest.tsv", &manifest);
}

fn champion_metadata_export(db: &ClientDatabase) -> serde_json::Value {
    let sheet = serde_json::to_value(&db.champion_info_sheet).unwrap_or_default();
    let mod_champions = sheet
        .get("mod_champions")
        .and_then(serde_json::Value::as_array);
    let champions = db
        .available_champions
        .iter()
        .map(|id| {
            let info = sheet.get(id).or_else(|| {
                mod_champions.and_then(|entries| {
                    entries.iter().find(|entry| {
                        entry.get("id").and_then(serde_json::Value::as_str) == Some(id.as_str())
                    })
                })
            });
            let category = info
                .and_then(|value| value.get("category"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("Unknown");
            let raw_tags: Vec<String> = info
                .and_then(|value| value.get("tags"))
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_string)
                .collect();
            let role_fit = derive_role_fit(category, &raw_tags);
            serde_json::json!({
                "id": id,
                "category": category,
                "rawTags": raw_tags,
                "roleFit": role_fit,
            })
        })
        .collect::<Vec<_>>();

    serde_json::json!({
        "schemaVersion": 1,
        "champions": champions,
    })
}

// The game does not expose numeric role-fit values. This deliberately small,
// reviewable prior uses only its category and raw tags. The Coach blends it as
// eight virtual games, so actual role usage continuously replaces it.
fn derive_role_fit(category: &str, tags: &[String]) -> BTreeMap<&'static str, f64> {
    let has = |tag: &str| {
        tags.iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(tag))
    };
    let melee = category.eq_ignore_ascii_case("melee") || has("Melee");
    let ranged = category.eq_ignore_ascii_case("range")
        || category.eq_ignore_ascii_case("ranged")
        || has("Range");

    let mut scores = BTreeMap::from([
        ("top", 20.0),
        ("jungle", 20.0),
        ("mid", 20.0),
        ("bot", 20.0),
        ("support", 20.0),
    ]);
    let mut add = |role: &'static str, value: f64| {
        if let Some(score) = scores.get_mut(role) {
            *score += value;
        }
    };

    if melee {
        add("top", 25.0);
        add("jungle", 20.0);
    }
    if ranged {
        add("mid", 15.0);
        add("bot", 35.0);
        add("support", 10.0);
    }
    if has("AD") {
        add("top", 15.0);
        add("jungle", 15.0);
        add("bot", 35.0);
    }
    if has("AP") || has("Magic") {
        add("mid", 35.0);
        add("jungle", 10.0);
        add("support", 10.0);
    }
    if has("Tank") {
        add("top", 30.0);
        add("jungle", 15.0);
        add("support", 25.0);
    }
    if has("CC") {
        add("top", 10.0);
        add("jungle", 25.0);
        add("mid", 10.0);
        add("support", 25.0);
    }
    if has("Heal") {
        add("support", 45.0);
    }
    if has("Shield") {
        add("support", 45.0);
    }

    let peak = scores.values().copied().fold(0.0_f64, f64::max);
    if peak > 0.0 {
        for score in scores.values_mut() {
            *score = (*score / peak * 1000.0).round() / 10.0;
        }
    }
    scores
}

fn athlete_mastery_export(data: &ClientData) -> serde_json::Value {
    let rows: Vec<serde_json::Value> = data
        .athlete_ids()
        .into_iter()
        .filter_map(|athlete_id| {
            let athlete = data.athlete(athlete_id)?;
            Some(serde_json::json!({
                "id": athlete.id,
                "name": athlete.name,
                "contract": &athlete.contract,
                "stats": &athlete.stat,
                "recentChampions": &athlete.recent_champions,
                "championProficiency": &athlete.champion_proficiency,
            }))
        })
        .collect();

    serde_json::json!({
        "schemaVersion": 2,
        "athletes": rows,
    })
}

fn champion_balance_history(db: &ClientDatabase, campaign_key: &str) -> serde_json::Value {
    let current_date = db.time.date();
    let current_version = db.version_at_date(current_date);
    let mut version_dates = BTreeMap::new();

    for row in &db.solo_rank_matches {
        let date = row.date.date();
        version_dates
            .entry(row.version.clone())
            .and_modify(|known| {
                // Use the last observed day of a historical patch. The first
                // day is a transition boundary and the game's date resolver
                // can already return the following balance sheet there.
                if date > *known {
                    *known = date;
                }
            })
            .or_insert(date);
    }
    version_dates.insert(current_version.clone(), current_date);

    for version in db.pre_patch_data.keys() {
        version_dates.entry(version.clone()).or_insert(current_date);
    }

    let captured = read_balance_snapshot_ledger(campaign_key);
    if let Some(entries) = captured.get("snapshots").and_then(|value| value.as_object()) {
        for version in entries.keys() {
            if valid_patch_version(version) {
                version_dates.entry(version.clone()).or_insert(current_date);
            }
        }
    }

    let anchor = db.available_champions.first().map(String::as_str);
    let mut snapshots = serde_json::Map::new();
    for (version, date) in version_dates {
        let captured_entry = captured
            .get("snapshots")
            .and_then(|value| value.get(&version));
        let (source, sheet, captured_roster) = if version == current_version {
            (
                "current",
                serde_json::to_value(&db.champion_info_sheet).unwrap_or_default(),
                Some(serde_json::to_value(&db.available_champions).unwrap_or_default()),
            )
        } else if let Some(state) = db.pre_patch_data.get(&version) {
            // The save-owned pre-patch snapshot wins over the external ledger;
            // older exporter builds may have captured a transient load state
            // under this same version.
            (
                "save",
                serde_json::to_value(&state.champion_info_sheet).unwrap_or_default(),
                Some(serde_json::to_value(&state.available_champions).unwrap_or_default()),
            )
        } else if let Some(entry) = captured_entry {
            (
                "captured",
                entry.get("championInfoSheet").cloned().unwrap_or_default(),
                entry.get("availableChampions").cloned(),
            )
        } else if let Some(anchor) = anchor {
            (
                "historical-untrusted",
                serde_json::to_value(db.get_historical_sheet(anchor, Some(date)))
                    .unwrap_or_default(),
                None,
            )
        } else {
            continue;
        };
        // Roster snapshots are authoritative for champion introductions. Some
        // historical balance sheets retain future champion keys, so key-set
        // differences alone cannot identify the patch that added a champion.
        // Unknown intermediate rosters stay null; the coach carries the last
        // known roster forward until the next authoritative snapshot.
        let available_champions = captured_roster;
        snapshots.insert(
            version,
            serde_json::json!({
                "date": date,
                "source": source,
                "availableChampions": available_champions,
                "championInfoSheet": sheet,
            }),
        );
    }

    serde_json::json!({
        "currentVersion": current_version,
        "generatedAt": db.time,
        "snapshots": snapshots,
    })
}

fn campaign_ledger_key(db: &ClientDatabase, player_team_id: usize) -> String {
    let earliest_match = db
        .solo_rank_matches
        .iter()
        .min_by_key(|row| row.id)
        .map(|row| format!("{}:{}:{}", row.id, row.version, row.date))
        .unwrap_or_else(|| "no-solo-match".to_string());
    let seed = format!("{}|{}|{}", db.id, player_team_id, earliest_match);
    // Stable FNV-1a keeps directory names short and needs no extra dependency.
    let hash = seed.bytes().fold(0xcbf29ce484222325_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
    });
    format!("v2-{hash:016x}")
}

fn balance_snapshot_ledger_path(campaign_key: &str) -> Option<String> {
    // Keep captures for each campaign permanently separate. The coach clears
    // only the one-shot export files before requesting a new export; these
    // ledgers survive save switching so returning to a campaign restores its
    // own patch-note history.
    coach_export_dir().map(|dir| format!("{dir}/balance_snapshots/{campaign_key}/ledger.json"))
}

fn read_balance_snapshot_ledger(campaign_key: &str) -> serde_json::Value {
    balance_snapshot_ledger_path(campaign_key)
        .and_then(|path| fs::read_to_string(path).ok())
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_else(|| serde_json::json!({ "snapshots": {} }))
}

fn valid_patch_version(version: &str) -> bool {
    version
        .split('.')
        .next()
        .and_then(|part| part.parse::<u64>().ok())
        .is_some_and(|year| year >= 2000)
}

fn capture_current_balance_snapshot(db: &ClientDatabase, campaign_key: &str) {
    let version = db.version_at_date(db.time.date());
    if !valid_patch_version(&version) || db.available_champions.is_empty() {
        return;
    }
    let Some(path) = balance_snapshot_ledger_path(campaign_key) else {
        return;
    };
    let mut ledger = read_balance_snapshot_ledger(campaign_key);
    let Some(snapshots) = ledger
        .get_mut("snapshots")
        .and_then(|value| value.as_object_mut())
    else {
        return;
    };
    snapshots.insert(
        version,
        serde_json::json!({
            "capturedAt": db.time,
            "availableChampions": &db.available_champions,
            "championInfoSheet": &db.champion_info_sheet,
        }),
    );
    if let Some(parent) = Path::new(&path).parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(serialized) = serde_json::to_string(&ledger) {
        let _ = fs::write(path, serialized);
    }
}

/// TSV-safe: strip tab/newline so a stray name can't break a row.
fn sanitize(value: &str) -> String {
    value.replace(['\t', '\n', '\r'], " ")
}

/// Write a file to the LT AI Coach data folder, where the Coach reads it.
fn write_file(file_name: &str, contents: &str) {
    let Some(dir) = coach_export_dir() else {
        return;
    };
    let path = format!("{dir}/{file_name}");
    if let Some(parent) = Path::new(&path).parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(&path, contents);
}

fn write_bytes(file_name: &str, contents: &[u8]) {
    let Some(dir) = coach_export_dir() else {
        return;
    };
    let path = format!("{dir}/{file_name}");
    if let Some(parent) = Path::new(&path).parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(path, contents);
}

fn init(_ctx: &GameCtx) -> ModRegistration {
    let mut reg = ModRegistration::new(MOD_ID);
    reg.set_extension(ExporterExtension);
    reg
}

declare_mod!(init);
