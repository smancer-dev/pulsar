// Translation catalogs for the runtime i18n in `i18n.svelte.ts`. The per-language
// strings live in `i18n.tr.ts` / `i18n.en.ts`; this module just assembles them
// into the active-language record and re-exports the shared types. Pure data only
// (no runes), so it stays a plain `.ts` module.

import { tr } from './i18n.tr';
import { en } from './i18n.en';
import type { Lang, Dict } from './i18n.types';

export type { Lang, Dict };

export const catalogs: Record<Lang, Dict> = { tr, en };
