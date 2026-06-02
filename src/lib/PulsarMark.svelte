<script lang="ts">
	let { size = 28, color = 'var(--accent)' }: { size?: number; color?: string } = $props();
</script>

<svg class="pulsar-mark" width={size} height={size} viewBox="0 0 52 52" fill="none" aria-hidden="true">
	<circle cx="26" cy="26" r="24" stroke={color} stroke-width="1.5" opacity="0.18" />
	<circle cx="26" cy="26" r="16" stroke={color} stroke-width="1.75" opacity="0.42" />
	<circle cx="26" cy="26" r="8.5" stroke={color} stroke-width="2" />
	<circle cx="26" cy="26" r="3.5" fill={color} />
</svg>
