// Shared i18n types, factored out so the per-language catalogs and the catalog
// barrel can import them without a circular dependency.

export type Lang = 'tr' | 'en';

export type Dict = Record<string, string>;
