import adapter from '@sveltejs/adapter-static';

/** @type {import('@sveltejs/kit').Config} */
const config = {
	compilerOptions: {
		runes: ({ filename }) => (filename.split(/[/\\]/).includes('node_modules') ? undefined : true)
	},
	kit: {
		// Tauri serves a static SPA bundle (no Node server on the device).
		adapter: adapter({ fallback: 'index.html' })
	}
};

export default config;
