(function () {
  const vscode = acquireVsCodeApi();

  function escapeHtml(s) { return String(s ?? '').replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c])); }
  function humanBytes(n) { if (n < 1024) return n + ' o'; if (n < 1048576) return (n/1024).toFixed(1) + ' Ko'; return (n/1048576).toFixed(1) + ' Mo'; }
  function formatDate(iso) { try { return new Date(iso).toLocaleString('fr-FR', { day:'2-digit', month:'short', hour:'2-digit', minute:'2-digit' }); } catch { return iso; } }

  function renderStatus(status, errKind) {
    const el = document.getElementById('statusCard');
    const quick = document.getElementById('quickActions');

    if (errKind === 'not-found') {
      el.innerHTML = `<div class="status-row">⚠️ <span>binaire <code>iloc</code> introuvable</span></div>`;
      quick.innerHTML = '';
      return;
    }
    if (!status || !status.initialised) {
      el.innerHTML = `<div class="status-row"><span class="pulse-dot off"></span> Pas encore un projet ilocker</div>`;
      quick.innerHTML = `<button class="quick-btn primary" data-cmd="iloc init">Initialiser ce dossier</button>`;
      wireQuickButtons();
      return;
    }

    const providers = [
      status.github_connected && 'GitHub',
      status.vercel_connected && 'Vercel',
      status.supabase_connected && 'Supabase',
    ].filter(Boolean);

    el.innerHTML = `
      <div class="status-row"><span class="pulse-dot${status.sentinel_active ? '' : ' amber'}"></span> Sentinel ${status.sentinel_active ? 'actif' : 'inactif'}</div>
      <div class="status-row"><span class="k">Snapshots</span> ${status.snapshot_count}</div>
      <div class="status-row"><span class="k">Providers</span> ${providers.length ? escapeHtml(providers.join(', ')) : 'aucun'}</div>
    `;

    const actions = [];
    if (status.snapshot_count === 0) { actions.push(['iloc save "premier snapshot"', 'Créer le premier snapshot']); }
    else { actions.push(['iloc save "snapshot"', 'Nouveau snapshot']); actions.push(['iloc status', 'Voir les changements']); }
    if (providers.length === 0) { actions.push(['iloc deploy', 'Connecter GitHub / Vercel / Supabase']); }
    if (!status.sentinel_active) { actions.push(['iloc sentinel enable', 'Activer le Sentinel']); }

    quick.innerHTML = actions.map(([cmd, label]) => `<button class="quick-btn" data-cmd="${escapeHtml(cmd)}">${escapeHtml(label)}</button>`).join('');
    wireQuickButtons();
  }

  function wireQuickButtons() {
    document.querySelectorAll('.quick-btn').forEach(b => {
      b.onclick = () => vscode.postMessage({ type: 'runCommand', commandLine: b.dataset.cmd });
    });
  }

  function renderRecent(snapshots) {
    const heading = document.getElementById('recentHeading');
    const el = document.getElementById('recentSnaps');
    if (!snapshots || snapshots.length === 0) { heading.style.display = 'none'; el.innerHTML = ''; return; }
    heading.style.display = 'block';
    el.innerHTML = snapshots.map(s => `<div class="mini-snap"><div class="msg">${escapeHtml(s.message)}</div><div class="meta">${formatDate(s.created_at)} · ${humanBytes(s.total_bytes)}</div></div>`).join('');
  }

  document.getElementById('openBtn').onclick = () => vscode.postMessage({ type: 'openCommandCenter' });

  window.addEventListener('message', event => {
    const msg = event.data;
    if (msg.type === 'projectStatus') { renderStatus(msg.data, null); }
    if (msg.type === 'projectStatusError') { renderStatus(null, msg.error); }
    if (msg.type === 'recentSnapshots') { renderRecent(msg.data); }
  });
})();
