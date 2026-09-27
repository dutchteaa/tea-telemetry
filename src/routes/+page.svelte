<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { onMount } from 'svelte';
  import { formatLapTime } from '$lib/format';

  type RecorderStatus = {
    state: 'no_sim' | 'idle' | 'recording';
    sim: 'iracing' | 'lmu' | null;
    lap: number | null;
    laps_saved: number;
    last_error: string | null;
  };

  type LapSummary = {
    lap_id: string;
    sim: string;
    track_name: string;
    track_config: string;
    car_name: string;
    session_type: string;
    lap_number: number;
    lap_time_ms: number;
    valid: boolean;
    invalid_reason: string | null;
    fuel_used_l: number | null;
    created_at_ms: number;
  };

  let status = $state<RecorderStatus | null>(null);
  let laps = $state<LapSummary[]>([]);
  let error = $state<string | null>(null);

  const simName = (s: string | null) => (s === 'iracing' ? 'iRacing' : s === 'lmu' ? 'LMU' : '');

  function statusText(s: RecorderStatus | null): string {
    if (!s || s.state === 'no_sim') return 'No sim detected';
    if (s.state === 'idle') return `${simName(s.sim)} · idle`;
    return `${simName(s.sim)} · recording · Lap ${s.lap ?? '?'}`;
  }

  async function refresh() {
    try {
      status = await invoke<RecorderStatus>('get_status');
      laps = await invoke<LapSummary[]>('recent_laps', { limit: 50 });
      error = null;
    } catch (e) {
      error = String(e);
    }
  }

  onMount(() => {
    refresh();
    const id = setInterval(refresh, 1000);
    return () => clearInterval(id);
  });
</script>

<main>
  <header>
    <h1>Tea Telemetry</h1>
    <span class="pill" class:live={status?.state === 'recording'} class:idle={status?.state === 'idle'}>
      {statusText(status)}
    </span>
  </header>

  {#if status?.last_error}
    <p class="error">Recorder: {status.last_error}</p>
  {/if}
  {#if error}
    <p class="error">{error}</p>
  {/if}

  <h2>Recent laps</h2>
  {#if laps.length === 0}
    <p class="muted">No laps yet. Start driving and completed laps will appear here.</p>
  {:else}
    <table>
      <thead>
        <tr><th>Track</th><th>Car</th><th>Session</th><th>Lap</th><th>Time</th><th>Fuel</th><th></th></tr>
      </thead>
      <tbody>
        {#each laps as lap (lap.lap_id)}
          <tr class:invalid={!lap.valid}>
            <td>{lap.track_name}{lap.track_config ? ` · ${lap.track_config}` : ''}</td>
            <td>{lap.car_name}</td>
            <td>{lap.session_type}</td>
            <td class="num">{lap.lap_number}</td>
            <td class="num">{formatLapTime(lap.lap_time_ms)}</td>
            <td class="num">{lap.fuel_used_l != null ? `${lap.fuel_used_l.toFixed(2)} L` : '–'}</td>
            <td class="muted">{lap.valid ? '' : lap.invalid_reason?.replace('_', ' ')}</td>
          </tr>
        {/each}
      </tbody>
    </table>
  {/if}
</main>

<style>
  :global(body) {
    margin: 0;
    background: #0e1116;
    color: #c9d1d9;
    font-family: 'Segoe UI', system-ui, sans-serif;
  }
  main { padding: 20px 24px; }
  header { display: flex; align-items: center; gap: 16px; }
  h1 { font-size: 20px; margin: 0; }
  h2 { font-size: 13px; text-transform: uppercase; letter-spacing: 0.06em; color: #6b7785; margin-top: 28px; }
  .pill { padding: 4px 12px; border-radius: 999px; font-size: 12px; background: #1f2630; color: #8b949e; }
  .pill.idle { background: rgba(56, 189, 248, 0.12); color: #38bdf8; }
  .pill.live { background: rgba(74, 222, 128, 0.14); color: #4ade80; }
  .error { color: #f87171; font-size: 13px; }
  .muted { color: #6b7785; }
  table { width: 100%; border-collapse: collapse; font-size: 13px; }
  th { text-align: left; color: #6b7785; font-weight: 600; padding: 6px 8px; border-bottom: 1px solid #1f2630; }
  td { padding: 6px 8px; border-bottom: 1px solid #161b22; }
  .num { font-variant-numeric: tabular-nums; }
  tr.invalid td:not(.muted) { color: #6b7785; }
</style>
