//! Backend i18n: user-visible strings the RUST side produces (error messages shown
//! in frontend modals, dialog window titles) live in `lang/tr.json` + `lang/en.json`
//! and are looked up by key via [`t`] — same model as the frontend's i18n.tr.ts /
//! i18n.en.ts and the renderer's `lang/*.json`. The active language follows
//! `Config.language` ([`set_lang`] runs at config load and on every set_config).
//! Missing keys fall back to Turkish (the app default), then to the key itself.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

static TR_JSON: &str = include_str!("../lang/tr.json");
static EN_JSON: &str = include_str!("../lang/en.json");

static ENGLISH: AtomicBool = AtomicBool::new(false);

/// Follow `Config.language` (En → English; anything else Turkish).
pub(crate) fn set_lang(language: &pulsar_core::config::Language) {
	ENGLISH.store(
		matches!(language, pulsar_core::config::Language::En),
		Ordering::Relaxed,
	);
}

/// The active language code for child processes (the renderer's `--lang`).
pub(crate) fn lang() -> &'static str {
	if ENGLISH.load(Ordering::Relaxed) {
		"en"
	} else {
		"tr"
	}
}

fn parse(json: &'static str) -> HashMap<&'static str, &'static str> {
	let v: serde_json::Value = serde_json::from_str(json).expect("lang json is valid");
	let obj = v.as_object().expect("lang json is an object");
	obj.iter()
		.filter_map(|(k, v)| {
			let s = v.as_str()?;
			// Leak once: small, static-for-life catalogs.
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

/// Localized string for `key` (EN → TR fallback → the key, so typos stay visible).
pub(crate) fn t(key: &str) -> &'static str {
	if ENGLISH.load(Ordering::Relaxed) {
		if let Some(s) = en().get(key) {
			return s;
		}
	}
	if let Some(s) = tr().get(key) {
		return s;
	}
	Box::leak(key.to_string().into_boxed_str())
}
