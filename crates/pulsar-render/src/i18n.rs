//! Native-renderer i18n: EVERY user-visible string lives in `lang/tr.json` +
//! `lang/en.json` (compiled in via `include_str!`), keyed lookups via [`t`]. The
//! renderer is a separate process, so the webview's i18n tables can't be shared —
//! the app passes its `Config.language` as `--lang tr|en` and [`set_english`]
//! selects the catalog. Missing keys fall back to Turkish (the app default), then
//! to the key itself so a typo is visible instead of silent.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

static TR_JSON: &str = include_str!("../lang/tr.json");
static EN_JSON: &str = include_str!("../lang/en.json");

static ENGLISH: AtomicBool = AtomicBool::new(false);

/// `--lang en` → English; anything else stays Turkish (the app default).
pub fn set_english(en: bool) {
	ENGLISH.store(en, Ordering::Relaxed);
}

fn parse(json: &'static str) -> HashMap<&'static str, &'static str> {
	let v: serde_json::Value = serde_json::from_str(json).expect("lang json is valid");
	let obj = v.as_object().expect("lang json is an object");
	obj.iter()
		.filter_map(|(k, v)| {
			let s = v.as_str()?;
			// Leak once at startup: the catalogs are small, static-for-life tables.
			Some((
				&*Box::leak(k.clone().into_boxed_str()),
				&*Box::leak(s.to_string().into_boxed_str()),
			))
		})
		.collect()
}

fn tr() -> &'static HashMap<&'static str, &'static str> {
	static T: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
	T.get_or_init(|| parse(TR_JSON))
}

fn en() -> &'static HashMap<&'static str, &'static str> {
	static T: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
	T.get_or_init(|| parse(EN_JSON))
}

/// Localized string for `key` in the active language (EN → TR fallback → the key).
pub fn t(key: &str) -> &'static str {
	if ENGLISH.load(Ordering::Relaxed) {
		if let Some(s) = en().get(key) {
			return s;
		}
	}
	if let Some(s) = tr().get(key) {
		return s;
	}
	// Visible-but-safe fallback for a missing key.
	Box::leak(key.to_string().into_boxed_str())
}
