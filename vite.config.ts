import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vite';

// Tauri expects a fixed dev port and ignores vite's HMR websocket env.
export default defineConfig({
	plugins: [sveltekit()],
	clearScreen: false,
	server: {
		port: 5173,
		strictPort: true,
		// Pre-transform the app shell before anything can request it.
		//
		// `tauri dev` polls for the port and opens the webview the moment vite is listening
		// — about a second after startup, and well before the module graph is warm. The
		// webview then asks for a component's `?svelte&type=style` CSS while
		// vite-plugin-svelte has not compiled that component yet, so its `load` misses
		// ("failed to load virtual css module"), vite falls through to its plain CSS plugin,
		// and PostCSS is handed the raw `.svelte` source — a burst of nonsense
		// "Unknown word onMount" errors on files nobody edited. A browser opened by hand a
		// few seconds later never reproduced it, which is what identified the race.
		//
		// Warming the layout + page (and, through them, the screens they import) fills that
		// cache during startup, so the first request already has a compiled component.
		warmup: {
			clientFiles: ['./src/routes/+layout.svelte', './src/routes/+page.svelte']
		}
	}
});
